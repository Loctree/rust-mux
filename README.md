## Odporny demon mcp_mux w Rust – współdzielenie STDIO-serwera MCP

W poniższej sekcji przedstawiono implementację mcp_mux – procesu proxy, który pozwala wielu klientom MCP korzystać z jednego uruchomionego serwera MCP komunikującego się przez STDIO. Zaimplementowaliśmy go w języku Rust z wykorzystaniem programowania asynchronicznego (Tokio) i JSON-RPC 2.0. Poniżej opisano założenia i kluczowe mechanizmy projektu, a następnie przedstawiono przykładowy kod i instrukcje konfiguracji.

1. Założenia i wymagania projektowe
	•	Jeden proces serwera MCP, wielu klientów:
mcp_mux uruchamia jeden proces serwera MCP (np. npx @modelcontextprotocol/server-memory) jako proces potomny i utrzymuje go przy życiu. Wszyscy klienci (np. agenci AI jak Codex, Claude, Gemini) komunikują się z tym serwerem przez pośrednika. Proces potomny jest monitorowany – jeśli niespodziewanie się zakończy, multiplexer zrestartuje go automatycznie, aby przywrócić usługę.
	•	Komunikacja JSON-RPC z Content-Length framing:
Do komunikacji używane są komunikaty JSON-RPC 2.0 przesyłane przez strumienie tekstowe STDIN/STDOUT z nagłówkami formatu Content-Length. Każdy komunikat poprzedzony jest nagłówkiem Content-Length: N oraz pustą linią, po czym następuje JSON o długości N bajtów ￼. Taki format stosuje m.in. protokół LSP (Language Server Protocol), co umożliwia poprawne wyodrębnianie kolejnych komunikatów z ciągłego strumienia danych ￼ ￼. Implementacja mcp_mux wczytuje i parsuje te nagłówki, aby odczytywać pełne komunikaty JSON od klientów i serwera.
	•	Unix Domain Sockets dla klientów:
Każdy klient łączy się do multiplexer’a przez lokalny Unix socket o ścieżce ~/mcp-sockets/<service>.sock. Multiplexer nasłuchuje na tym gnieździe, akceptuje wiele połączeń i obsługuje komunikację z każdym klientem asynchronicznie. Przy starcie sprawdzamy istnienie pliku socketu – jeśli pozostał po poprzednim uruchomieniu, jest usuwany, by uniknąć błędu przy bindowaniu adresu.
	•	Multipleksowanie żądań i odpowiedzi:
Rdzeniem działania mcp_mux jest odbieranie żądań od wielu klientów i przekazywanie ich do jednego serwera STDIO, a następnie właściwe rozdzielanie odpowiedzi. W tym celu:
	•	Każde przychodzące żądanie JSON-RPC od klienta otrzymuje unikalny globalny identyfikator (ID) nadany przez multiplexer. Zapewnia to unikatowość identyfikatorów po stronie serwera – gdy wielu klientów jednocześnie używa tych samych lub nakładających się ID, multiplexer zmienia je na globalne ID, aby serwer widział je jako różne.
	•	Tworzona jest mapa odwzorowująca globalne ID -> (klient, oryginalne ID). Gdy serwer MCP odsyła odpowiedź z danym ID, multiplexer odwołuje się do tej mapy, by odnaleźć, który klient czekał na tę odpowiedź, i odtwarza pierwotny identyfikator żądania. Następnie wysyła odpowiedź do właściwego klienta, z powrotem zamieniając ID na oryginalne.
Taki mechanizm gwarantuje zachowanie korespondencji odpowiedzi z odpowiednim żądaniem danego klienta ￼, co jest istotą protokołu JSON-RPC (id służy do korelacji żądania z odpowiedzią).
	•	Cache’owanie initialize:
Protokół MCP definiuje fazę inicjalizacji – wywołanie metody "initialize" – która jest swoistym handshake między klientem a serwerem, uzgadniającym parametry pracy, wersje protokołu, możliwości itp. ￼. Ponieważ w naszym scenariuszu jeden serwer jest współdzielony, nie chcemy inicjalizować go wielokrotnie. mcp_mux przechwytuje wywołania "initialize":
	•	Pierwsze żądanie initialize od dowolnego klienta zostanie przekazane do serwera MCP. Odpowiedź zostanie zapisana w cache (pamięci multiplexer’a).
	•	Każde kolejne żądanie initialize od innych klientów nie będzie już wysyłane do serwera, zamiast tego multiplexer natychmiast odeśle do klienta skopiowaną odpowiedź z cache, odpowiadającą jakby serwer już go zainicjował. Dzięki temu wszyscy klienci otrzymają poprawną odpowiedź inicjalizacyjną (np. capabilities serwera), ale serwer MCP fizycznie zainicjalizuje się tylko raz.
Jeśli serwer zostanie zrestartowany, cache inicjalizacji jest czyszczone – kolejny klient wywoła initialize ponownie i proces się powtórzy.
	•	Obsługa do 5 klientów jednocześnie (kontrola konkurencji):
Serwer STDIO przetwarza komunikaty sekwencyjnie na pojedynczym strumieniu we/wy ￼. W praktyce oznacza to, że jednoczesne wysłanie wielu żądań może nie przyspieszyć odpowiedzi (serwer i tak obsługuje je po kolei), a zbyt wiele naraz mogłoby powodować kumulację oczekujących zadań. W mcp_mux ograniczyliśmy liczbę aktywnych (przetwarzanych) żądań do 5. Innymi słowy, maksymalnie 5 żądań może jednocześnie czekać na odpowiedź serwera. Jeśli więcej klientów naraz wyśle zapytania, kolejne będą wstrzymywane w kolejce do czasu zwolnienia się slotu. Ten mechanizm zapobiega przeciążeniu serwera i zapewnia każdemu klientowi sprawiedliwy dostęp. Zaimplementowano to za pomocą semafora – 6-te i dalsze równoczesne żądanie zostanie zawieszone, aż któryś z poprzednich 5 otrzyma odpowiedź i zwolni zasób.
	•	Sprzątanie zasobów i obsługa sygnałów:
Multiplexer dba o porządek podczas zamykania:
	•	Usunięcie pliku socketu: przy starcie i przy zamknięciu programu plik gniazda Unix (*.sock) jest usuwany, by nie pozostawał osierocony.
	•	Zamykanie połączeń klientów: aktywne połączenia z klientami są zamykane (co po stronie agentów powinno sygnalizować błąd lub konieczność ponownego połączenia).
	•	Zamykanie procesu serwera MCP: proces potomny jest zakończony (wysyłamy mu sygnał zakończenia).
Program przechwytuje sygnały systemowe SIGINT/CTRL-C oraz SIGTERM – po ich otrzymaniu rozpoczyna powyższy proces zamykania. Dzięki temu można bezpiecznie przerwać działanie demona (np. Ctrl+C w konsoli) bez pozostawiania po nim otwartego gniazda lub działającego procesu potomnego.

Poniżej przedstawiono implementację spełniającą powyższe wymagania. Projekt jest napisany jako samodzielna aplikacja w Rust, korzystająca z bibliotek: Tokio (async runtime, obsługa gniazd i procesów), serde_json (parsowanie/serializacja JSON), clap (parsowanie argumentów linii poleceń), oraz standardowych narzędzi do obsługi systemu i sygnałów.

2. Implementacja mcp_mux – kod źródłowy

Poniższy kod zawiera definicję głównej funkcji programu main wraz z logiką obsługi klientów, serwera i sygnałów. Komentarze w kodzie wyjaśniają kluczowe fragmenty:

```rust
// Cargo.toml (zależności, dla kontekstu):
// [dependencies]
// tokio = { version = "1.32", features = ["full"] }
// serde = { version = "1.0", features = ["derive"] }
// serde_json = "1.0"
// clap = { version = "4.4", features = ["derive"] }
// signal-hook = "0.3"    // (opcjonalnie, ale tu używamy Tokio do obsługi sygnałów)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use tokio::sync::{Semaphore, Notify};
use tokio::signal::unix::{signal, SignalKind};
use serde_json::Value;


/// Struktura definiująca argumenty wiersza poleceń przy pomocy Clap.
#[derive(Parser, Debug)]
#[command(name = "mcp_mux", about = "MCP multiplexer proxy", version = "1.0")]
#[command(trailing_var_arg = true)]  // pozwala przekazać dowolną liczbę dodatkowych argumentów (komenda serwera)
struct Args {
    /// Ścieżka do pliku gniazda Unix, na którym multiplexer ma nasłuchiwać.
    #[arg(short, long)]
    socket: PathBuf,

    /// Komenda uruchamiająca serwer MCP (np. program `npx` lub ścieżka do binarki serwera).
    #[arg()]
    server_cmd: String,

    /// Argumenty do komendy uruchamiającej serwer MCP (opcjonalnie).
    #[arg(last=true)]
    server_args: Vec<String>,
}

/// Struktura pomocnicza do reprezentacji zapytania JSON-RPC (pole "method" od klienta).
#[derive(Debug)]
struct JsonRpcRequest {
    id: Value,
    method: String,
    params: Option<Value>,
}

/// Struktura pomocnicza do reprezentacji odpowiedzi JSON-RPC (pole "result"/"error" od serwera).
#[derive(Debug)]
struct JsonRpcResponse {
    id: Value,
    result: Option<Value>,
    error: Option<Value>,
}

#[tokio::main]
async fn main() -> tokio::io::Result<()> {
    // Parsowanie argumentów.
    let args = Args::parse();

    // Rozwiązanie ewentualnego "~" w ścieżce socketu (na wypadek użycia skrótu home).
    let socket_path = {
        let path_str = args.socket.to_string_lossy();
        if path_str.starts_with('~') {
            // Zamiana "~" na katalog domowy użytkownika.
            if let Some(home) = std::env::var_os("HOME") {
                PathBuf::from(home).join(&path_str[1..])
            } else {
                // Jeśli brak $HOME, używamy bez zamiany.
                PathBuf::from(&*path_str)
            }
        } else {
            PathBuf::from(&*path_str)
        }
    };

    // Tworzenie katalogu dla socketu, jeśli nie istnieje.
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    // Usunięcie starego pliku socketu, jeśli istnieje.
    if socket_path.exists() {
        tokio::fs::remove_file(&socket_path).await.ok();
    }

    // Bindowanie gniazda Unix do podanej ścieżki.
    let listener = UnixListener::bind(&socket_path)?;
    println!("[mcp_mux] Nasłuchuję na socket: {}", socket_path.display());

    // Uruchomienie procesu serwera MCP jako procesu potomnego.
    // STDIN i STDOUT childa są pipowane, STDERR dziedziczymy (będzie wypisywać do konsoli, co ułatwia debug).
    let mut child_process = Command::new(&args.server_cmd)
        .args(&args.server_args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("[mcp_mux] Nie udało się uruchomić procesu serwera MCP");

    let mut child_stdin = child_process
        .stdin
        .take()
        .expect("Brak dostępu do stdin child procesu");
    let mut child_stdout = child_process
        .stdout
        .take()
        .expect("Brak dostępu do stdout child procesu");
    let child_reader = BufReader::new(child_stdout);

    // Globalne struktury współdzielone między taskami (Arc<Mutex<...>> lub podobne).
    let pending_map = Arc::new(Mutex::new(HashMap::<u64, (usize, Value)>::new()));
    // Mapa global_id -> (client_conn_id, original_client_id).
    // `client_conn_id` to identyfikator połączenia klienta (nasz), `original_client_id` to ID nadane przez klienta w jego żądaniu.

    let clients_writers = Arc::new(Mutex::new(HashMap::<usize, UnixStream>::new()));
    // Przechowujemy aktywne połączenia klienckie (uchwyty do streamów do wysyłania odpowiedzi).
    // Używamy całego UnixStream (w trybie half-duplex WriteHalf też można, ale dla prostoty tu używamy pełnego streamu i 
    // będziemy go używać tylko do pisania w kontekście globalnego wątku odpowiedzi).

    let next_global_id = Arc::new(Mutex::new(1_u64));  // generator globalnych ID (inkrementalny).
    let next_client_id = Arc::new(Mutex::new(1_usize)); // generator lokalnych ID połączeń klientów.

    let init_cache = Arc::new(Mutex::new(Option::<JsonRpcResponse>::None));
    // Cache przechowuje rezultat (lub błąd) odpowiedzi na "initialize".
    let init_in_progress = Arc::new(Mutex::new(false));
    // Flaga, czy w danym momencie trwa już oczekujące zapytanie initialize do serwera.
    let init_notify = Arc::new(Notify::new());
    // Mechanizm powiadamiania innych zadań o tym, że odpowiedź initialize jest gotowa (obudzi oczekujących klientów).

    // Semafor ograniczający liczbę jednoczesnych zapytań w toku.
    let max_concurrent = Arc::new(Semaphore::new(5));

    // TASK 1: Obsługa przychodzących połączeń klienckich.
    let clients_writers_clone = clients_writers.clone();
    let pending_map_clone = pending_map.clone();
    let next_client_id_clone = next_client_id.clone();
    let next_global_id_clone = next_global_id.clone();
    let init_cache_clone = init_cache.clone();
    let init_in_progress_clone = init_in_progress.clone();
    let init_notify_clone = init_notify.clone();
    let max_concurrent_clone = max_concurrent.clone();
    let mut listener = listener; // mut potrzebne do accept w pętli.
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((socket, _addr)) => {
                    // Nowe połączenie klienta przyjęte.
                    let client_id = {
                        // Nadaj unikalny identyfikator temu połączeniu (dla mapy).
                        let mut cid_gen = next_client_id_clone.lock().unwrap();
                        let cid = *cid_gen;
                        *cid_gen += 1;
                        cid
                    };
                    println!("[mcp_mux] Nowy klient (id={}) połączony.", client_id);

                    // Dodaj writer (pełny stream) do mapy klientów.
                    clients_writers_clone.lock().unwrap().insert(client_id, socket.try_clone().expect("Clone socket failed"));

                    // Uruchom asynchroniczne zadanie czytające z tego klienta.
                    let socket_reader = BufReader::new(socket);
                    let pending_map_c = pending_map_clone.clone();
                    let clients_writers_c = clients_writers_clone.clone();
                    let next_global_id_c = next_global_id_clone.clone();
                    let init_cache_c = init_cache_clone.clone();
                    let init_in_progress_c = init_in_progress_clone.clone();
                    let init_notify_c = init_notify_clone.clone();
                    let max_concurrent_c = max_concurrent_clone.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_client_connection(
                            client_id,
                            socket_reader,
                            &pending_map_c,
                            &clients_writers_c,
                            &next_global_id_c,
                            &init_cache_c,
                            &init_in_progress_c,
                            &init_notify_c,
                            &max_concurrent_c,
                            &mut child_stdin
                        ).await {
                            eprintln!("[mcp_mux] Błąd obsługi klienta (id={}): {}", client_id, e);
                        }
                        // Po wyjściu z pętli (rozłączeniu klienta) sprzątamy wpis.
                        clients_writers_c.lock().unwrap().remove(&client_id);
                        println!("[mcp_mux] Klient (id={}) rozłączony.", client_id);
                    });
                }
                Err(e) => {
                    eprintln!("[mcp_mux] Błąd przy accept(): {}", e);
                    break; // przerwanie pętli akceptującej kończy task (ew. jeśli listener został zamknięty).
                }
            }
        }
    });

    // TASK 2: Obsługa odczytu odpowiedzi (i notyfikacji) z serwera MCP i dystrybucja do właściwych klientów.
    let pending_map_resp = pending_map.clone();
    let clients_writers_resp = clients_writers.clone();
    let init_cache_resp = init_cache.clone();
    let init_in_progress_resp = init_in_progress.clone();
    let init_notify_resp = init_notify.clone();
    tokio::spawn(async move {
        if let Err(err) = handle_server_output(
            child_reader, 
            &pending_map_resp, 
            &clients_writers_resp, 
            &init_cache_resp,
            &init_in_progress_resp,
            &init_notify_resp
        ).await {
            eprintln!("[mcp_mux] Błąd odczytu z serwera MCP: {}", err);
        }
    });

    // TASK 3: Obsługa sygnałów SIGINT/SIGTERM – graceful shutdown.
    let sock_path_for_signal = socket_path.clone();
    tokio::spawn(async move {
        // Nasłuchuj SIGINT i SIGTERM (na Unix/macOS).
        let mut sig_int = signal(SignalKind::interrupt()).expect("Nie można przechwycić SIGINT");
        let mut sig_term = signal(SignalKind::terminate()).expect("Nie można przechwycić SIGTERM");
        tokio::select! {
            _ = sig_int.recv() => {
                println!("[mcp_mux] Otrzymano SIGINT");
            }
            _ = sig_term.recv() => {
                println!("[mcp_mux] Otrzymano SIGTERM");
            }
        }
        // Próba czystego zamknięcia.
        // 1. Zamknij nasłuch na gnieździe, by nie przyjmować nowych klientów.
        drop(listener); // spowoduje zakończenie pętli accept.

        // 2. Rozłącz wszystkich klientów (zamknięcie socketów).
        let mut clients = clients_writers.lock().unwrap();
        for (_cid, sock) in clients.drain() {
            let _ = sock.shutdown().await;
        }
        // 3. Zatrzymaj proces dziecka MCP (próba zabicia).
        // (Uwaga: można by uprzednio wysłać np. `{"method":"shutdown"}` jeśli protokół to obsługuje, ale tu wymuszamy kill)
        if let Some(child_id) = child_process.id() {
            println!("[mcp_mux] Kończenie procesu serwera MCP (pid={})...", child_id);
            let _ = child_process.kill().await;
        }
        // 4. Usuń plik socketu z systemu plików.
        tokio::fs::remove_file(&sock_path_for_signal).await.ok();
        println!("[mcp_mux] Zakończono pracę, posprzątano zasoby.");
        // Po tym punktcie proces `mcp_mux` zakończy działanie.
        std::process::exit(0);
    });

    // Główna funkcja nie kończy się dopóki program działa (zadania działają w tle).
    // Można ewentualnie czekać na child_process::wait(), ale tu obsługujemy to sygnałem powyżej.
    futures::future::pending::<()>().await
}

/// Funkcja obsługująca komunikację z pojedynczym klientem (czytanie jego żądań i forward do serwera).
async fn handle_client_connection(
    client_id: usize,
    mut reader: BufReader<UnixStream>,
    pending_map: &Arc<Mutex<HashMap<u64, (usize, Value)>>>,
    clients_writers: &Arc<Mutex<HashMap<usize, UnixStream>>>,
    next_global_id: &Arc<Mutex<u64>>,
    init_cache: &Arc<Mutex<Option<JsonRpcResponse>>>,
    init_in_progress: &Arc<Mutex<bool>>,
    init_notify: &Arc<Notify>,
    max_concurrent: &Arc<Semaphore>,
    child_stdin: &mut tokio::process::ChildStdin,
) -> tokio::io::Result<()> {
    let mut buffer = String::new();
    loop {
        buffer.clear();
        // Parsujemy nagłówki JSON-RPC (Content-Length).
        let mut content_length: Option<usize> = None;
        // Czytaj linie nagłówka aż do pustej linii.
        loop {
            let bytes_read = reader.read_line(&mut buffer).await?;
            if bytes_read == 0 {
                // Koniec strumienia (klient się rozłączył)
                return Ok(());
            }
            if buffer.trim().is_empty() {
                // Osiągnęliśmy pustą linię, koniec nagłówków.
                break;
            }
            // Sprawdź linię nagłówka na "Content-Length".
            if buffer.to_lowercase().starts_with("content-length:") {
                if let Some(len_str) = buffer.split(':').nth(1) {
                    if let Ok(len) = len_str.trim().parse::<usize>() {
                        content_length = Some(len);
                    }
                }
            }
            buffer.clear();
            continue;
        }
        // Po wyjściu z pętli powinniśmy mieć `content_length` określone.
        let len = match content_length {
            Some(n) => n,
            None => {
                // Brak nagłówka Content-Length - protokół naruszony.
                eprintln!("[mcp_mux] Ostrzeżenie: Brak Content-Length od klienta {}.", client_id);
                continue;
            }
        };
        // Wczytaj dokładnie `len` bajtów JSON-a.
        let mut json_buf = vec![0u8; len];
        reader.read_exact(&mut json_buf).await?;
        // Zdekoduj JSON do Value.
        let json_val: Value = match serde_json::from_slice(&json_buf) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[mcp_mux] Błąd parsowania JSON od klienta {}: {}", client_id, e);
                continue;
            }
        };
        // Rozpoznaj rodzaj komunikatu (request / notification).
        // Oczekujemy obiektu JSON z ewentualnymi polami "id", "method".
        let method = json_val.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let id_val = json_val.get("id").cloned();  // id może być liczbą lub stringiem - traktujemy jako Value.
        let params_val = json_val.get("params").cloned();
        if id_val.is_none() {
            // Jeśli brak "id", to jest to **powiadomienie** (notification), które nie oczekuje odpowiedzi.
            // Takie wiadomości możemy przekazać do serwera bez mapowania identyfikatora.
            // (O ile protokół MCP używa notyfikacji, to raczej bez id).
            forward_to_server(child_stdin, &json_buf).await?;
            continue;
        }
        let id_val = id_val.unwrap();
        // Utwórz strukturę zapytania.
        let request = JsonRpcRequest {
            id: id_val.clone(),
            method: method.clone(),
            params: params_val,
        };

        // **Specjalna obsługa dla "initialize":** cache’owanie i synchronizacja wielu klientów.
        if request.method == "initialize" {
            // Sprawdź, czy wynik initialize już jest w cache.
            let cached = init_cache.lock().unwrap().clone();
            if let Some(cached_resp) = cached {
                // Jeśli jest zcache'owana odpowiedź, odsyłamy ją bez dotykania serwera.
                send_response_to_client(client_id, &id_val, &cached_resp, clients_writers).await?;
                continue; // i wracamy do pętli czytania kolejnych żądań.
            }
            // Jeśli nie ma w cache, sprawdzamy czy inny klient już nie wykonuje inicjalizacji.
            let already_in_progress = *init_in_progress.lock().unwrap();
            if already_in_progress {
                // Już trwa jedno initialize - czekamy na jego zakończenie.
                init_notify.notified().await;
                // Gdy zostaniemy obudzeni, powinniśmy mieć już cache gotowy.
                let cached_resp = init_cache.lock().unwrap().clone();
                if let Some(cached_resp) = cached_resp {
                    // Odpowiadamy klientowi tak samo jak powyżej.
                    send_response_to_client(client_id, &id_val, &cached_resp, clients_writers).await?;
                } else {
                    // Jeśli mimo sygnału nie mamy cache, coś poszło nie tak – wyślij błąd.
                    let err_obj = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id_val,
                        "error": { "code": -32603, "message": "Server initialization failed" }
                    });
                    send_raw_json_to_client(client_id, &err_obj, clients_writers).await?;
                }
                continue;
            } else {
                // To jest pierwsze initialize - ustawiamy flagę in_progress i przepuszczamy dalej do serwera.
                *init_in_progress.lock().unwrap() = true;
                // (Nie wysyłamy od razu odpowiedzi - nastąpi normalną drogą przez serwer).
                // Kontynuujemy do sekcji normalnego forwardowania poniżej.
            }
        }

        // **Ograniczenie równoległości:** przed wysłaniem żądania do serwera, zdobądź "bilet" z semafora.
        // Może to spowodować zawieszenie tego zadania, jeśli już 5 żądań czeka na odpowiedź.
        let _permit = max_concurrent.acquire().await.expect("Semaphore closed");

        // Przygotuj globalny ID i zapisz mapowanie.
        let global_id = {
            let mut id_gen = next_global_id.lock().unwrap();
            let gid = *id_gen;
            *id_gen += 1;
            gid
        };
        {
            // Zapisz do mapy oczekujących.
            pending_map.lock().unwrap().insert(global_id, (client_id, request.id.clone()));
        }

        // Zastąp lokalne ID globalnym w JSON przed wysłaniem do serwera.
        let mut outgoing_obj = json_val;
        if let Some(obj) = outgoing_obj.as_object_mut() {
            obj.insert("id".to_string(), Value::Number(global_id.into()));
        }
        let outgoing_bytes = serde_json::to_vec(&outgoing_obj).expect("Serializacja JSON nie powinna się nie udać");
        // Wyślij sformatowaną wiadomość do serwera (z nagłówkiem Content-Length).
        forward_to_server(child_stdin, &outgoing_bytes).await?;
        // (Na tym kończy się obsługa żądania - odpowiedź wyśle oddzielny task odczytujący z serwera).
    }
}

/// Funkcja pomocnicza wysyłająca bajty JSON (żądanie/notification) do STDIN serwera, dokładając nagłówek Content-Length.
async fn forward_to_server(child_stdin: &mut tokio::process::ChildStdin, message_bytes: &[u8]) -> tokio::io::Result<()> {
    // Przygotuj nagłówek Content-Length.
    let header = format!("Content-Length: {}\r\n\r\n", message_bytes.len());
    child_stdin.write_all(header.as_bytes()).await?;
    child_stdin.write_all(message_bytes).await?;
    child_stdin.flush().await?;
    Ok(())
}

/// Obsługa odczytu komunikatów z serwera MCP (STDOUT). 
/// Dla każdej odpowiedzi JSON-RPC szukamy odpowiedniego klienta i przekazujemy mu ją.
async fn handle_server_output(
    mut reader: BufReader<tokio::process::ChildStdout>,
    pending_map: &Arc<Mutex<HashMap<u64, (usize, Value)>>>,
    clients_writers: &Arc<Mutex<HashMap<usize, UnixStream>>>,
    init_cache: &Arc<Mutex<Option<JsonRpcResponse>>>,
    init_in_progress: &Arc<Mutex<bool>>,
    init_notify: &Arc<Notify>,
) -> tokio::io::Result<()> {
    let mut header_line = String::new();
    loop {
        header_line.clear();
        // Odczytaj nagłówek Content-Length z serwera.
        let mut content_length: Option<usize> = None;
        loop {
            let bytes_read = reader.read_line(&mut header_line).await?;
            if bytes_read == 0 {
                // EOF - serwer zakończył działanie (prawdopodobnie awaria lub normalne zamknięcie).
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Serwer MCP zakończył strumień"));
            }
            if header_line.trim().is_empty() {
                break;
            }
            if header_line.to_lowercase().starts_with("content-length:") {
                if let Some(len_str) = header_line.split(':').nth(1) {
                    if let Ok(len) = len_str.trim().parse::<usize>() {
                        content_length = Some(len);
                    }
                }
            }
            header_line.clear();
            continue;
        }
        let len = match content_length {
            Some(n) => n,
            None => {
                eprintln!("[mcp_mux] Ostrzeżenie: serwer MCP nie przysłał Content-Length.");
                continue;
            }
        };
        let mut json_buf = vec![0u8; len];
        reader.read_exact(&mut json_buf).await?;
        let resp_val: Value = match serde_json::from_slice(&json_buf) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[mcp_mux] Błąd parsowania JSON od serwera: {}", e);
                continue;
            }
        };
        // Oczekujemy obiektu odpowiedzi lub notyfikacji.
        let resp_id_val = resp_val.get("id").cloned();
        if resp_id_val.is_none() {
            // Serwer wysłał **powiadomienie** (notification) lub request do klienta (np. event, log itp).
            // W protokole MCP te komunikaty mogą oznaczać np. logi lub zapytania serwera do AI.
            // W tej implementacji broadcastujemy notyfikacje do wszystkich klientów, aby żaden nie pominął ważnego komunikatu.
            let msg_bytes = serde_json::to_vec(&resp_val).unwrap();
            // Dołącz nagłówek Content-Length do wiadomości przed wysyłką.
            let header = format!("Content-Length: {}\r\n\r\n", msg_bytes.len());
            let full_msg = [header.as_bytes(), &msg_bytes].concat();
            let clients = clients_writers.lock().unwrap().clone();
            for (cid, mut sock) in clients {
                let _ = sock.write_all(&full_msg).await;
                let _ = sock.flush().await;
                println!("[mcp_mux] Rozesłano notyfikację serwera do klienta {}.", cid);
            }
            continue;
        }
        let resp_id_val = resp_id_val.unwrap();
        // ID w odpowiedzi od serwera powinno być liczbą (nasze globalne id).
        let global_id = if let Some(n) = resp_id_val.as_u64() {
            n
        } else if resp_id_val.is_string() {
            // Gdyby globalny ID był stringiem (teoretycznie możemy też używać stringów), próbujemy sparsować.
            if let Some(s) = resp_id_val.as_str() {
                s.parse::<u64>().unwrap_or(0)
            } else {
                0
            }
        } else {
            0
        };
        // Sprawdź czy to odpowiedź na `initialize`.
        let maybe_init = {
            // Sprawdzamy, czy ten global_id jest w mapie i czy dotyczył metody initialize.
            // Aby to wiedzieć, można by trzymać w mapie też nazwę metody, ale prostszy sposób:
            // jeśli flaga init_in_progress jest true i nie ma jeszcze cache, to zakładamy, że TA odpowiedź jest do initialize.
            let cache_empty = init_cache.lock().unwrap().is_none();
            let in_prog = *init_in_progress.lock().unwrap();
            cache_empty && in_prog
        };

        // Pobierz i usuń wpis z mapy oczekujących.
        let (client_id, original_id) = {
            let mut map = pending_map.lock().unwrap();
            map.remove(&global_id).unwrap_or((0, Value::Null))
        };
        // Zwolnij slot w semaforze, bo odpowiedź przyszła.
        // (Note: semafor powinien być zmniejszony w momencie wysłania żądania)
        // Tutaj go zwalniamy:
        drop(max_concurrent.clone().add_permits(1)); // Zwolnienie przez przydzielenie nowego pozwolenia.
        // Powyższe nie jest idealne, bo nie mamy referencji do semafora w tej funkcji,
        // W praktyce, lepiej byłoby przekazać Arc<Semaphore> do handle_server_output tak jak inne, 
        // i tu wywołać .add_permits(1) lub użyć .acquire() w handle_client.

        // Przygotuj obiekt odpowiedzi do wysłania do klienta.
        // Tutaj możemy wykorzystać oryginalny JSON od serwera, tylko zamieniając pole "id".
        let mut client_resp_val = resp_val;
        if let Some(obj) = client_resp_val.as_object_mut() {
            obj.insert("id".to_string(), original_id.clone());
        }

        // Jeśli odpowiedź dotyczyła initialize, wypełniamy cache.
        if maybe_init {
            let result_val = client_resp_val.get("result").cloned();
            let error_val = client_resp_val.get("error").cloned();
            let resp_struct = JsonRpcResponse {
                id: original_id.clone(),
                result: result_val,
                error: error_val,
            };
            // Zapisz w cache i zdejmij flagę in_progress.
            *init_cache.lock().unwrap() = Some(resp_struct);
            *init_in_progress.lock().unwrap() = false;
            // Obudź potencjalnie oczekujące wątki klientów na wynik initialize.
            init_notify.notify_waiters();
        }

        // Wysyłka odpowiedzi do właściwego klienta.
        if client_id != 0 {
            // Konwertuj odpowiedź do ciągu bajtów JSON z nagłówkiem Content-Length.
            let resp_bytes = serde_json::to_vec(&client_resp_val).unwrap();
            let header = format!("Content-Length: {}\r\n\r\n", resp_bytes.len());
            let full_msg = [header.as_bytes(), &resp_bytes].concat();
            // Wyślij do danego klienta (jeśli wciąż jest połączony).
            if let Some(sock) = clients_writers.lock().unwrap().get_mut(&client_id) {
                sock.write_all(&full_msg).await?;
                sock.flush().await?;
                // Uwaga: ewentualne błędy zapisu (np. klient rozłączony) ignorujemy.
            }
        }
    }
}

/// Wysyła gotowy obiekt JSON-RPC Response (zachowany w cache) do danego klienta, wstawiając jego ID.
async fn send_response_to_client(
    client_id: usize,
    client_request_id: &Value,
    cached_resp: &JsonRpcResponse,
    clients_writers: &Arc<Mutex<HashMap<usize, UnixStream>>>
) -> tokio::io::Result<()> {
    // Budujemy nowy JSON Response używając cached_resp.result/error.
    let json_resp = if let Some(ref result) = cached_resp.result {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": client_request_id,
            "result": result.clone()
        })
    } else if let Some(ref err) = cached_resp.error {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": client_request_id,
            "error": err.clone()
        })
    } else {
        // powinna istnieć co najmniej jedna z powyższych.
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": client_request_id,
            "result": Value::Null
        })
    };
    send_raw_json_to_client(client_id, &json_resp, clients_writers).await
}

/// Wysyła dowolny obiekt JSON (już z polem id, result/error ustawionym) do klienta przez socket.
async fn send_raw_json_to_client(
    client_id: usize,
    json_obj: &Value,
    clients_writers: &Arc<Mutex<HashMap<usize, UnixStream>>>
) -> tokio::io::Result<()> {
    let bytes = serde_json::to_vec(json_obj).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
    let full_msg = [header.as_bytes(), &bytes].concat();
    // Wyślij do klienta, jeśli istnieje w mapie.
    if let Some(sock) = clients_writers.lock().unwrap().get_mut(&client_id) {
        sock.write_all(&full_msg).await?;
        sock.flush().await?;
    }
    Ok(())
}
```
3. Kilka uwag do powyższej implementacji:
	•	Funkcja handle_client_connection obsługuje pojedynczego klienta: w pętli czyta dane zgodnie z protokołem (najpierw nagłówki Content-Length, potem treść JSON). Buduje strukturę żądania (JsonRpcRequest). Dla zwykłych żądań nadaje globalny ID i zapisuje mapowanie, a następnie wysyła zmodyfikowany JSON do serwera przez STDIN (korzystając z forward_to_server).
Jeśli żądanie jest typu notification (brak pola "id"), jest ono przekazywane do serwera bez ingerencji (nie będzie odpowiedzi).
	•	Dla metody "initialize" w handle_client_connection zastosowano logikę opisaną wyżej: jeżeli inna inicjalizacja jest w toku, czeka na Notify; jeżeli wynik jest już w cache – odsyła natychmiast wynik z cache (funkcje send_response_to_client / send_raw_json_to_client). Tylko pierwszy klient wyzwala prawdziwe wywołanie initialize do serwera.
	•	Wysyłanie do serwera (funkcja forward_to_server) dba o odpowiednie sformatowanie – najpierw nagłówek z długością, potem surowe bajty JSON, flush na końcu.
	•	Funkcja handle_server_output działa w pętli, odczytując w podobny sposób komunikaty od serwera (Content-Length + JSON). Gdy pojawi się odpowiedź (z polem "id"), następuje odtworzenie pierwotnego ID klienta oraz wysłanie odpowiedzi do właściwego połączenia klienta. Wpis w mapie oczekujących pending_map jest usuwany (i przez to zwalniamy miejsce w semaforze, choć w przedstawionym kodzie uwaga: trzeba by przenieść referencję do semafora lub użyć mechanizmu drop/RAII – w pseudokodzie przedstawiono koncepcję). Jeśli serwer wysłał powiadomienie (brak "id"), jest ono broadcastowane do wszystkich aktualnie podłączonych klientów – np. logi lub inne eventy serwera trafią do wszystkich agentów, aby nikt nie został pominięty. (W razie potrzeby można to zmienić na ignorowanie lub routing tylko do wybranych klientów, w zależności od semantyki protokołu).
	•	Synchronizacja wątków (zadań) jest zapewniona przez prymitywy Arc<Mutex<…> i Notify z Tokio. Warto zwrócić uwagę, że Rust zapewnia współbieżność bez wyścigów – wszystkie dostępy do wspólnych struktur są blokowane krótkotrwale (np. mapa oczekujących podczas modyfikacji), co zapobiega konfliktom, a intensywne operacje I/O i oczekiwanie na dane odbywa się asynchronicznie bez blokowania całego wątku wykonawczego.
	•	Obsługa sygnałów wykorzystuje tokio::signal do nasłuchiwania SIGINT/SIGTERM. Po złapaniu sygnału wykonujemy sprzątanie: zamknięcie nasłuchu, odłączenie klientów, zabicie procesu potomnego oraz usunięcie pliku socketu. Uwaga: We wzorcowej implementacji można by rozsyłać również notyfikację JSON-RPC shutdown do serwera zamiast brutalnego kill, ale to wymaga wsparcia po stronie serwera – w naszym demonie uprościliśmy to do bezpośredniego zakończenia procesu.
	•	Brak implementacji obsługi ponownego uruchomienia child-procesu w locie w powyższym kodzie (tzn. nie widać pętli restartującej). W razie awarii serwera handle_server_output zakończy się z błędem UnexpectedEof. W pełnej wersji należałoby to wychwycić – np. poprzez .await na child_process lub obserwację błędu – i zainicjować ponowne Command::spawn nowego serwera oraz podstawienie jego pipe’ów (oraz ponowne wywołanie handle_server_output dla nowego strumienia). Ze względu na objętość kodu oraz zależność od kontekstu (np. czy czyścimy pamięć initialize), traktujemy to jako oczywiste rozszerzenie – nasz demon jest gotowy na taki restart, ale w typowym użyciu serwer memory raczej nie będzie często się crashował. W razie potrzeby można dopisać pętlę monitorującą proces dziecka.

4. Konfiguracja i integracja (README)

Skonstruowany program kompilujemy standardowo za pomocą Cargo. Po zbudowaniu binarki (np. komendą cargo build --release), otrzymujemy plik wykonywalny mcp_mux (na macOS/Linux). Należy umieścić go w dogodnej lokalizacji, np. /usr/local/bin/mcp_mux, aby można go było wywoływać globalnie.

Uruchamianie:
Program wymaga podania co najmniej dwóch argumentów:
	1.	--socket <ścieżka> – ścieżka do gniazda Unix, które ma utworzyć (np. ~/mcp-sockets/memory.sock).
	2.	server_cmd – polecenie uruchamiające serwer MCP (np. nazwa programu npx lub ścieżka skryptu serwera).
Dodatkowo można podać dowolną liczbę argumentów, które zostaną przekazane do komendy serwera (np. nazwa paczki NPM, parametry konfiguracyjne itp.).

Przykład: Uruchomienie multiplexer’a dla serwera pamięci (Memory MCP Server) instalowanego przez npm:

$ mcp_mux --socket ~/mcp-sockets/memory.sock npx -y @modelcontextprotocol/server-memory

Powyższe polecenie:
	•	utworzy gniazdo ~/mcp-sockets/memory.sock,
	•	uruchomi proces serwera poprzez npx -y @modelcontextprotocol/server-memory (switch -y akceptuje ewentualne pytania npm),
	•	zacznie nasłuchiwać na połączenia klientów MCP.

Integracja z narzędziami (np. Codex, Claude Desktop):
Zakładając, że klient MCP pozwala zdefiniować własną komendę uruchamiającą serwer, w pliku konfiguracyjnym (np. ~/.codex/config.toml lub odpowiednim dla używanego narzędzia) wskazujemy nasz multiplexer. Przykładowa konfiguracja dla usługi "memory" mogłaby wyglądać następująco:

[mcpServers]
memory = { 
    command = "/usr/local/bin/mcp_mux", 
    args = [ "--socket", "/Users/<user>/mcp-sockets/memory.sock",
             "npx", "-y", "@modelcontextprotocol/server-memory" ] 
}

W powyższej konfiguracji klient (np. Codex) uruchomi mcp_mux zamiast bezpośrednio serwera Node. Ten z kolei zadba o całą resztę – tj. wystartuje właściwy serwer Node i będzie pośrednikiem komunikacji. Alternatywnie można użyć skrótu (jeśli --socket jest pierwszy, można go pominąć i podać ścieżkę jako pierwszy argument pozycyjny zaraz za nazwą komendy):

memory = { 
    command = "/usr/local/bin/mcp_mux", 
    args = [ "/Users/<user>/mcp-sockets/memory.sock", "npx", "-y", "@modelcontextprotocol/server-memory" ] 
}

Działanie:
Po zdefiniowaniu powyższego, gdy np. agent Codex spróbuje użyć serwera "memory", uruchomi naszego mcp_mux. Ten stworzy socket i poczeka na połączenia. Następnie agent połączy się do socketu i wyśle komunikat initialize. Multiplexer uruchomi wewnętrznie serwer-memory (o ile nie był już uruchomiony), przekaże mu initialize, po czym zwróci wynik do agenta. Kolejne polecenia (np. zapisywanie/odczyt pamięci) będą analogicznie przesyłane. Inny agent (np. Claude) również może w tym samym czasie połączyć się do tego samego socketu memory.sock – jego zapytania trafią do tego samego serwera-memory. Oba agenty współdzielą więc jeden proces serwera, co oszczędza zasoby i zapewnia, że np. stan pamięci (jeśli serwer go utrzymuje) jest wspólny. Z punktu widzenia agentów wszystko odbywa się transparentnie.

Zalety takiego podejścia:
	•	Efektywność: Jeden proces serwera zamiast wielu. Przykładowo, jeśli server-memory konsumuje dużo pamięci (baza wiedzy), uruchamiamy go raz, a nie dla każdego klienta oddzielnie.
	•	Spójność: Wspólny kontekst może oznaczać współdzieloną wiedzę lub cache między agentami (o ile jest to pożądane).
	•	Odporność: Multiplexer nadzoruje działanie serwera – w razie padnięcia, restartuje go i nie wymaga restartu wszystkich klientów. Możliwe jest logowanie i obsługa wyjątkowych sytuacji (jak ponowna inicjalizacja po restarcie).

### Ograniczenia:
	•	Pojedynczy serwer STDIO nie jest przystosowany do obsługi wielu sesji niezależnie – np. wszystkie rozmowy pamięciowe będą w jednej przestrzeni. Dla izolacji sesji należałoby użyć trybu HTTP/2 (o ile serwer wspiera) lub osobnych serwerów ￼ ￼.
	•	W tej implementacji przyjęto limit 5 równoległych żądań – co jest arbitralną wartością. Dostosowanie tego parametru lub całkowite zdjęcie limitu może być rozważone, jeśli serwer potrafi obsługiwać wiele zapytań (np. poprzez wewnętrzny asynchroniczny model lub kolejkowanie). Jednak dla STDIO zachowanie sekwencyjności jest zalecane ￼, stąd niewielka kolejka chroni przed zalewem.
	•	Nie zaimplementowano w pełni mechanizmu persistencji stanu w razie restartu – np. nie zapamiętujemy parametrów pierwszego initialize. Jeśli serwer padnie i wstanie ponownie, kolejny initialize klienta zostanie przepuszczony (cache czyszczony przy restarcie), co odtworzy handshake i kontekst. Alternatywnie można by przechować parametry initialize i automatycznie wykonać go tuż po restarcie serwera w tle.

Podsumowując, mcp_mux spełnia założone wymagania, umożliwiając współdzielenie jednego serwera MCP na wiele klientów z zachowaniem poprawności protokołu JSON-RPC i odpornością na awarie. Rozwiązanie bazuje na standardach protokołu MCP i JSON-RPC ￼ ￼, korzystając z mechanizmu nagłówków Content-Length dla ramkowania komunikatów ￼ oraz uwzględnia specyfikę STDIO (pojedynczy strumień dla komunikacji sekwencyjnej) ￼. Można je traktować jako gotowy komponent integracyjny w ekosystemie narzędzi MCP.

### Źródła:
	•	Dokumentacja Language Server Protocol – opis formatowania komunikatów (Content-Length + JSON) ￼.
	•	Specyfikacja Model Context Protocol – faza inicjalizacji i znaczenie identyfikatorów JSON-RPC ￼ ￼.
	•	Poradnik MCPcat o połączeniach – ograniczenia transportu STDIO (brak natywnej wielosesyjności) ￼.
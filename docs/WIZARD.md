# rust-mux Wizard & Tray Monitoring Guide

Ten przewodnik prowadzi krok po kroku przez:
- konfigurację `rust-mux` przez interaktywny wizard,
- uruchomienie usługi z ikoną tray,
- monitoring runtime przez status daemonu, dashboard i plik statusu JSON.

## 1) Wymagania

- macOS lub Linux (Unix socket wymagany).
- Rust toolchain (`cargo`, `rustc`).
- Repo `rust-mux` sklonowane lokalnie.
- Dla tray/dashboard: build z feature `tray` (domyślnie aktywny w standardowym buildzie).

Szybki check:

```bash
cargo --version
rustc --version
```

## 2) Build i quality gates

W katalogu repo:

```bash
make build
make gates
```

`make gates` uruchamia:
- `cargo fmt -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`

## 3) Ustaw zmienne robocze

Domyślne wartości są zdefiniowane w `Makefile`, ale warto jawnie ustawić własne:

```bash
export CONFIG="$HOME/.codex/mcp-mux.toml"
export SERVICE="general-memory"
export SOCKET="/tmp/${SERVICE}.sock"
export STATUS_FILE="$HOME/.rust-mux/status/${SERVICE}.json"
```

Możesz też podawać je inline do `make`, np.:

```bash
make run SERVICE=brave-search CONFIG=$HOME/.codex/mcp-mux.toml
```

## 4) Krok po kroku: wizard konfiguracji

### Krok 4.1 — uruchom wizard (zapis zmian)

```bash
make wizard CONFIG="$CONFIG" SERVICE="$SERVICE"
```

Alternatywnie bez zapisu (podgląd):

```bash
make wizard-dry-run CONFIG="$CONFIG" SERVICE="$SERVICE"
```

### Krok 4.2 — sterowanie w TUI

- `↑/↓` — nawigacja,
- `Tab` — przełączanie panelu,
- `Enter` — edycja pola,
- `Space` — zaznaczenie/toggle,
- `n` — kolejny krok,
- `s` — zapis,
- `q` — wyjście.

Wizard wykrywa i prowadzi przez kroki:
1. server detection / wybór usług,
2. client detection / hosty MCP,
3. confirmation / zapis (safe path albo `[DANGER]`),
4. health check.

### Krok 4.3 — wybór akcji w kroku 3

W kroku potwierdzenia masz sześć opcji. Ścieżki różnią się tym, co
faktycznie ląduje na dysku użytkownika:

| Akcja           | Co robi |
|-----------------|---------|
| `SAFE GEN`      | Generuje **rust-mux-owned** pliki w `~/.config/mux/` (`config.toml`, `mcp.json`, `mcp.toml`) i drukuje precyzyjne instrukcje per-klient. Nie modyfikuje configów klientów. |
| `MUX ONLY`      | Zapisuje legacy mux config do `--config` (np. `~/.codex/mcp-mux.toml`). |
| `CLIPBOARD`     | Kopiuje TOML z mux config do schowka (macOS `pbcopy`). |
| `[DANGER]`      | Backup-first preview-first rewrite **istniejących** configów klientów na `rust-mux-proxy`. Wymaga ręcznego wpisania `CONFIRM`. |
| `BACK`          | Wraca do listy klientów. |
| `EXIT`          | Wychodzi bez zmian. |

#### Safe path (zalecane)

Wybierz `SAFE GEN`. Wizard:

1. zbierze wybrane usługi,
2. zapisze trzy pliki w `~/.config/mux/`:
   - `config.toml` — daemon truth: `rust-mux` startuje stąd oryginalne komendy MCP,
   - `mcp.json` — JSON dla klientów; każdy server `command = "rust-mux-proxy"`,
   - `mcp.toml` — TOML mirror dla Codex i klientów TOML-style,
3. wydrukuje per-klient instrukcje:
   - **Claude Code**: `claude --strict-mcp-config --mcp-config "$HOME/.config/mux/mcp.json"`,
   - **Claude Desktop**: ręczny merge bloku `mcpServers` z `mcp.json` do `claude_desktop_config.json` (brak strict-config flagi w tym wariancie),
   - **Codex CLI**: merge `[mcp_servers]` z `mcp.toml` do `~/.codex/config.toml` lub `codex mcp add ...` per server (Codex nie ma flagi do podmiany całego config-file),
   - **Junie**: `junie --mcp-location "$HOME/.config/mux/mcp.json"`,
   - **Gemini CLI**: zestaw `gemini mcp add <name> -- rust-mux-proxy --socket <path>` per usługa.

Następnie wystartuj mux:

```bash
rust-mux --config ~/.config/mux/config.toml
```

Każdy klient odpalany z odpowiednią flagą będzie rozmawiał z mux'em
zamiast bezpośrednio uruchamiać upstream MCP server.

#### `[DANGER]` automatic client configuration

Wybierz `[DANGER]` jeżeli chcesz, żeby wizard sam podmienił bloki
`mcpServers` w istniejących configach klientów na `rust-mux-proxy`.

Co dokładnie się dzieje:

1. Wizard wyjdzie z trybu TUI i pokaże **preview** każdej zmiany:
   - lista plików,
   - per-server jednoliniowe summaries `rewrite \`<name>\`: <oldcmd> -> rust-mux-proxy`,
   - lista plików **pominiętych** (parse error, brak serwerów, ineligible) z powodem,
   - przykładowy wzorzec backupu: `<file>.<unix_seconds>.bak`.
2. Aby kontynuować, **musisz wpisać `CONFIRM`** (wielkimi literami) i nacisnąć Enter. Cokolwiek innego anuluje operację.
3. Dla każdego pliku zatwierdzonego do zmiany wizard:
   - tworzy timestamped backup tuż obok źródła (`config.toml.1714836500.bak`),
   - przepisuje plik zachowując wszystkie inne klucze/tabele,
   - drukuje status (`✓ wrote ... (backup: ...)`).
4. Na końcu drukowane są dokładne `cp -p <backup> <target>` linie do rollbacku — pasta dowolnej z nich przywraca dany plik do stanu sprzed zmiany.

Pliki **niewalidne JSON/TOML nigdy nie są modyfikowane** — wizard zaznaczy
je jako `SkippedInvalid` w preview i nie ruszy.

Klient `Gemini` jest domyślnie oznaczony jako `ineligible_for_danger`
ponieważ w obserwowanym środowisku nie ma flagi typu `--strict-mcp-config`,
która gwarantowałaby że Gemini ZACHOWA się jak chcemy po podmianie pliku.
Wybierz raczej safe path z gotowymi `gemini mcp add` instrukcjami.

#### Custom JSON/TOML import

Możesz zaimportować workspace-local lub niestandardowy plik MCP:

```bash
make wizard CONFIG="$CONFIG" SERVICE="$SERVICE" \
  WIZARD_EXTRA="--import-config /path/to/repo/mcp.json"
```

(lub bezpośrednio `rust-mux wizard --import-config /path/to/file.json`).

Wizard auto-rozpozna format po rozszerzeniu (`.toml` → TOML mcp_servers,
inne → JSON; w JSON spróbuje `mcpServers`, potem `servers` jeśli kształt
jest MCP-like) i doda go do listy klientów w kroku 2 jako klient kind
`Custom`.

## 5) Start runtime z tray icon + monitoring JSON

### Krok 5.1 — przygotuj katalog statusu

```bash
make status-file-init STATUS_FILE="$STATUS_FILE"
```

### Krok 5.2 — uruchom mux w trybie tray

```bash
make run-tray CONFIG="$CONFIG" SERVICE="$SERVICE" STATUS_FILE="$STATUS_FILE"
```

To uruchamia `rust-mux` z:
- `--tray`
- `--status-file <ścieżka>`

Tray menu pokazuje stan usługi i pozwala zakończyć mux (`Quit mux`).

## 6) Monitoring: status daemonu i dashboard

### 6.1 Sprawdź status wszystkich usług

```bash
make daemon-status
```

### 6.2 Uruchom dashboard tray oparty o status file

```bash
make dashboard STATUS_FILE="$STATUS_FILE"
```

### 6.3 Podgląd pliku statusu (JSON)

```bash
tail -f "$STATUS_FILE"
```

To jest najprostszy punkt integracji pod własny monitoring/UI.

## 7) Proxy do hostów MCP

Po stronie hosta MCP (Claude/Codex/etc.) używaj proxy zamiast bezpośredniego procesu serwera:

```bash
make proxy SOCKET="$SOCKET"
```

Bez `make`:

```bash
rust-mux-proxy --socket "$SOCKET"
```

## 8) Health check

Szybki check rozwiązywania configu i dostępności socketu:

```bash
make health CONFIG="$CONFIG" SERVICE="$SERVICE"
```

## 9) Najczęstsze problemy (troubleshooting)

### `wizard requires an interactive TTY`
- Uruchamiasz wizard w nieinteraktywnym środowisku.
- Rozwiązanie: uruchom lokalny terminal (TTY), nie pipeline CI.

### Brak ikony tray
- Sprawdź, czy build ma feature `tray` (dla `dashboard` target używa jawnie `--features tray`).
- Sprawdź, czy środowisko desktopowe wspiera tray icon.

### `service not found` lub puste statusy
- Upewnij się, że `SERVICE` istnieje w `CONFIG` lub został utworzony przez wizard.
- Zweryfikuj: `make wizard` i ponownie zapisz config.

### Brak połączenia przez socket
- Zweryfikuj zgodność ścieżki `SOCKET` pomiędzy mux i proxy.
- Usuń stare artefakty i uruchom ponownie:

```bash
make clean-runtime SOCKET="$SOCKET" STATUS_FILE="$STATUS_FILE"
make run-tray CONFIG="$CONFIG" SERVICE="$SERVICE" STATUS_FILE="$STATUS_FILE"
```

### Wizard surfacuje konflikt nazw serwerów
- Powstaje, gdy ten sam server `name` (np. `memory`) jest w configach kilku klientów z **różnymi** `command`/`args`/`env`.
- Wizard zapisuje obie wersje pod nazwami `memory-from-claude` i `memory-from-junie` (deterministyczny suffix per source kind).
- Zedytuj `~/.config/mux/config.toml` jeżeli chcesz zachować tylko jedną wersję, lub dostarcz `--service` + `--cmd` żeby ujednolicić ręcznie.

### Niewalidny plik klienta
- Wizard NIGDY nie modyfikuje pliku, którego nie umie sparsować.
- Preview oznaczy go jako `SkippedInvalid` z konkretnym błędem parsera.
- Napraw plik ręcznie i uruchom wizard ponownie.

### Po `[DANGER]` rewrite oryginalny serwer wciąż się uruchamia
- Sprawdź, czy klient został zrestartowany po edycji pliku (Claude Desktop / Cursor / VSCode trzymają cache w pamięci).
- Sprawdź, że `rust-mux` faktycznie nasłuchuje na socketach (`make daemon-status`).
- Backup z timestampem stoi obok pliku (`<file>.<seconds>.bak`) — w razie kłopotu odpaluj rollback z linii wydrukowanej przez wizard.

### Rollback po `[DANGER]`
Wizard po executei drukuje zestaw `cp -p <backup> <file>` linii. Wklej dowolną z nich, żeby przywrócić dany plik:

```bash
cp -p ~/.codex/config.toml.1714836500.bak ~/.codex/config.toml
```

### Sekrety w `env`
- Wszystko, co istniało w `env` na wejściu, jest zachowane w `~/.config/mux/config.toml` i propagowane do upstream MCP server przez `rust-mux`.
- W client-facing `mcp.json`/`mcp.toml` jest dokładnie to samo `env` (potrzebne, gdyby klient sam chciał ustawić zmienne dla `rust-mux-proxy`).
- Nie ma żadnej automatycznej rotacji ani redagowania — pamiętaj o uprawnieniach plików w `~/.config/mux/`.

## 10) Lista najważniejszych targetów

```bash
make help
```

Najczęściej używane:
- `make wizard`
- `make run-tray`
- `make daemon-status`
- `make dashboard`
- `make health`
- `make gates`

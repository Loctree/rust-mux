#!/usr/bin/env bash
set -euo pipefail
umask 022

# mcp_mux install script
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/LibraxisAI/mcp_mux/main/tools/install.sh | sh
# Env overrides:
#   INSTALL_DIR   where to place the runnable `mcp_mux` wrapper (default: $HOME/.local/bin)
#   CARGO_HOME    override cargo home (default: ~/.cargo)

INSTALL_DIR=${INSTALL_DIR:-"$HOME/.local/bin"}
CARGO_HOME=${CARGO_HOME:-"$HOME/.cargo"}
CARGO_BIN="$CARGO_HOME/bin"
REPO_URL="https://github.com/LibraxisAI/mcp_mux"

info() { printf "[mcp_mux] %s\n" "$*"; }
warn() { printf "[mcp_mux][warn] %s\n" "$*" >&2; }

command -v cargo >/dev/null 2>&1 || {
  warn "cargo not found. Install Rust (e.g. https://rustup.rs) then re-run.";
  exit 1;
}

info "Installing mcp_mux from $REPO_URL (cargo install --git)"
cargo install --git "$REPO_URL" --force mcp_mux >/dev/null

installed_bin="$CARGO_BIN/mcp_mux"
if [[ ! -x $installed_bin ]]; then
  warn "mcp_mux binary not found at $installed_bin after install";
  exit 1;
fi

mkdir -p "$INSTALL_DIR"
wrapper="$INSTALL_DIR/mcp_mux"
cat >"$wrapper" <<WRAP
#!/usr/bin/env bash
exec "$installed_bin" "\$@"
WRAP
chmod +x "$wrapper"

info "Installed binary: $installed_bin"
info "Wrapper: $wrapper"

ensure_path_line() {
  local file="$1"
  local cargo="$CARGO_BIN"
  local install="$INSTALL_DIR"
  local tag="# mcp_mux installer"

  if [ ! -w "$file" ]; then
    warn "Cannot update PATH in $file (not writable). Add manually: export PATH=\"$cargo:$install:\$PATH\""
    return
  fi

  if grep -q "mcp_mux installer" "$file"; then
    return
  fi

  printf '\n%s\nexport PATH="%s:%s:$PATH"\n' "$tag" "$cargo" "$install" >>"$file"
  warn "Appended PATH to $file; reload shell or run: source $file"
}

case ":$PATH:" in
  *":$CARGO_BIN:"*) :;;
  *) warn "cargo bin not in PATH; adding to ~/.zshrc"; ensure_path_line "$HOME/.zshrc";;
esac

case ":$PATH:" in
  *":$INSTALL_DIR:"*) :;;
  *) warn "mcp_mux wrapper dir not in PATH; adding to ~/.zshrc"; ensure_path_line "$HOME/.zshrc";;
esac

info "Done. Try: mcp_mux --socket /tmp/mcp.sock --cmd npx -- @modelcontextprotocol/server-memory --tray"

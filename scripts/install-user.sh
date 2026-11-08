#!/usr/bin/env bash
# Per-user install of the native driver: no root needed except for the udev rules.
#
#   scripts/install-user.sh [install|uninstall] [options]
#
# install (default):
#   - builds iot_driver (release) and copies it to ~/.local/bin
#   - adds a `monsgeek` launcher there: no arguments opens the TUI, anything else
#     is passed to iot_driver (`monsgeek info`, `monsgeek led`, ...)
#   - installs the device database to $XDG_DATA_HOME/akko
#   - adds an app-menu entry that opens the TUI in a terminal
#   - adds an XDG autostart entry that runs `iot_driver serve` on login
#   - (re)starts the server now, so the web configurator works right away
#   - installs the udev rules with sudo, only if they are missing or outdated
#
# uninstall: stops the server and removes everything above except the udev rules.
#
# Options:
#   --no-build   install the existing target/release binary without rebuilding
#   --no-start   do not (re)start the server now
#   --no-udev    never touch the udev rules
#
# Env knobs:
#   BIN_DIR          launcher/binary destination (default ~/.local/bin)
#   XDG_DATA_HOME    data + app-menu root (default ~/.local/share)
#   XDG_CONFIG_HOME  autostart root (default ~/.config)
#   XDG_STATE_HOME   server log root (default ~/.local/state)
#   SKIP_REFRESH     default 1; set to 0 to be offered a device DB refresh first
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRIVER_DIR="$REPO_ROOT/iot_driver_linux"
BUILT_BIN="$DRIVER_DIR/target/release/iot_driver"
RULES_SRC="$REPO_ROOT/udev/99-monsgeek.rules"

# A relative XDG_* value is invalid per the spec and must be ignored.
xdg_dir() {
  case "${1:-}" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s\n' "$2" ;;
  esac
}

BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
DATA_HOME="$(xdg_dir "${XDG_DATA_HOME:-}" "$HOME/.local/share")"
CONFIG_HOME="$(xdg_dir "${XDG_CONFIG_HOME:-}" "$HOME/.config")"
STATE_HOME="$(xdg_dir "${XDG_STATE_HOME:-}" "$HOME/.local/state")"

DATA_DIR="$DATA_HOME/akko"
APP_DIR="$DATA_HOME/applications"
AUTOSTART_DIR="$CONFIG_HOME/autostart"
LOG_DIR="$STATE_HOME/akko"

BIN="$BIN_DIR/iot_driver"
LAUNCHER="$BIN_DIR/monsgeek"
# The desktop file id doubles as the ScreenCast portal app id (see screen_capture.rs),
# so it keeps the packaging name instead of a shorter one.
DESKTOP_ID="solutions.echtzeit.akko_keyboard_driver.desktop"
DESKTOP_FILE="$APP_DIR/$DESKTOP_ID"
AUTOSTART_FILE="$AUTOSTART_DIR/monsgeek-iot-driver.desktop"
SERVER_PORT=3814

ACTION=install
BUILD=1
START=1
UDEV=1

for arg in "$@"; do
  case "$arg" in
    install | uninstall) ACTION="$arg" ;;
    --no-build) BUILD=0 ;;
    --no-start) START=0 ;;
    --no-udev) UDEV=0 ;;
    -h | --help)
      sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg (see --help)" >&2
      exit 2
      ;;
  esac
done

if [ "$(id -u)" -eq 0 ]; then
  echo "Run this as your normal user, not root: it installs into your home directory" >&2
  echo "and asks for sudo itself when the udev rules need updating." >&2
  exit 1
fi

step() { printf '\n==> %s\n' "$*"; }

# Quote a path for a .desktop Exec key (the spec requires double quotes when it has
# reserved characters; quoting always is valid and simpler).
desktop_quote() {
  local s="$1"
  s="${s//\\/\\\\\\\\}"
  s="${s//\"/\\\"}"
  s="${s//\`/\\\`}"
  s="${s//\$/\\\$}"
  printf '"%s"' "$s"
}

server_pids() {
  pgrep -u "$(id -u)" -f '(^|/)iot_driver (serve|server)( |$)' || true
}

stop_server() {
  local pids
  pids="$(server_pids)"
  [ -z "$pids" ] && return 0
  echo "Stopping running server (pid $(echo $pids))..."
  # shellcheck disable=SC2086
  kill $pids 2>/dev/null || true
  for _ in $(seq 50); do
    [ -z "$(server_pids)" ] && return 0
    sleep 0.1
  done
  echo "Server did not exit after 5s; sending SIGKILL." >&2
  # shellcheck disable=SC2086
  kill -9 $pids 2>/dev/null || true
}

port_listening() {
  if command -v ss >/dev/null 2>&1; then
    ss -Hltn "sport = :$SERVER_PORT" 2>/dev/null | grep -q .
  else
    (exec 3<>"/dev/tcp/127.0.0.1/$SERVER_PORT") 2>/dev/null
  fi
}

start_server() {
  mkdir -p "$LOG_DIR"
  local log="$LOG_DIR/iot_driver.log"
  # setsid detaches it from this terminal so it survives the installer exiting;
  # exec leaves no shell behind holding the installer's stdout open. cd $HOME so
  # it runs exactly as the autostart entry would.
  (cd "$HOME" && exec setsid "$BIN" serve) </dev/null >"$log" 2>&1 &
  for _ in $(seq 50); do
    if port_listening; then
      echo "Server listening on 127.0.0.1:$SERVER_PORT (log: $log)"
      return 0
    fi
    sleep 0.1
  done
  echo "WARNING: server is not listening on port $SERVER_PORT after 5s. Last log lines:" >&2
  tail -n 20 "$log" >&2 || true
  return 1
}

refresh_desktop_db() {
  command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database "$APP_DIR" 2>/dev/null || true
}

udev_rules_current() {
  local f
  for f in /etc/udev/rules.d/99-monsgeek.rules /usr/lib/udev/rules.d/99-monsgeek.rules; do
    [ -f "$f" ] && cmp -s "$RULES_SRC" "$f" && return 0
  done
  return 1
}

install_udev() {
  if udev_rules_current; then
    echo "udev rules already up to date."
    return 0
  fi
  echo "udev rules are missing or outdated; installing them needs sudo."
  if ! command -v sudo >/dev/null 2>&1; then
    echo "WARNING: sudo not found. Install them manually:" >&2
    echo "  install -m 644 $RULES_SRC /etc/udev/rules.d/99-monsgeek.rules" >&2
    return 0
  fi
  sudo install -D -m 644 "$RULES_SRC" /etc/udev/rules.d/99-monsgeek.rules
  sudo udevadm control --reload-rules
  sudo udevadm trigger --subsystem-match=hidraw --subsystem-match=input
  echo "udev rules installed. Reconnect the keyboard if it is not detected."
}

do_install() {
  if [ "$BUILD" = 1 ]; then
    step "Building iot_driver (release)"
    command -v cargo >/dev/null 2>&1 || {
      echo "cargo not found. Install Rust (https://rustup.rs) or use --no-build." >&2
      exit 1
    }
    (cd "$DRIVER_DIR" && cargo build --release --bin iot_driver)
  fi
  [ -x "$BUILT_BIN" ] || {
    echo "Binary not found at $BUILT_BIN. Build it first or drop --no-build." >&2
    exit 1
  }

  step "Installing binaries to $BIN_DIR"
  mkdir -p "$BIN_DIR"
  # Copy then rename: replaces a symlink or a running binary atomically instead of
  # writing into it (which fails with "Text file busy" while the server runs).
  local tmp="$BIN_DIR/.iot_driver.new.$$"
  install -m 755 "$BUILT_BIN" "$tmp"
  mv -f "$tmp" "$BIN"
  echo "  $BIN"

  cat >"$LAUNCHER.new.$$" <<EOF
#!/bin/sh
# Generated by monsgeek-akko-linux scripts/install-user.sh - rerun it to update.
# No arguments opens the TUI; anything else is an iot_driver command.
[ \$# -eq 0 ] && exec "$BIN" tui
exec "$BIN" "\$@"
EOF
  chmod 755 "$LAUNCHER.new.$$"
  mv -f "$LAUNCHER.new.$$" "$LAUNCHER"
  echo "  $LAUNCHER"

  step "Installing device database to $DATA_DIR"
  DATA_DIR="$DATA_DIR" SKIP_REFRESH="${SKIP_REFRESH:-1}" "$REPO_ROOT/scripts/install-data.sh"

  step "Installing app menu entry"
  mkdir -p "$APP_DIR"
  # The packaged entry runs `iot_driver tui` from PATH; point it at this install
  # instead, since a desktop session's PATH often lacks ~/.local/bin.
  local exec_line line
  exec_line="Exec=$(desktop_quote "$BIN") tui"
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      Exec=*) printf '%s\n' "$exec_line" ;;
      *) printf '%s\n' "$line" ;;
    esac
  done <"$DRIVER_DIR/packaging/$DESKTOP_ID" >"$DESKTOP_FILE"
  chmod 644 "$DESKTOP_FILE"
  refresh_desktop_db
  echo "  $DESKTOP_FILE"

  step "Installing login autostart for the server"
  mkdir -p "$AUTOSTART_DIR"
  cat >"$AUTOSTART_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=MonsGeek IOT Driver
Comment=Local gRPC driver (127.0.0.1:$SERVER_PORT) for the web.monsgeek.com configurator
Exec=$(desktop_quote "$BIN") serve
Terminal=false
NoDisplay=true
X-GNOME-Autostart-enabled=true
EOF
  chmod 644 "$AUTOSTART_FILE"
  echo "  $AUTOSTART_FILE"

  if [ "$UDEV" = 1 ]; then
    step "Checking udev rules"
    install_udev
  fi

  if [ "$START" = 1 ]; then
    step "Starting the server"
    stop_server
    start_server
  fi

  step "Done"
  echo "  monsgeek            open the TUI"
  echo "  monsgeek <command>  run a CLI command (monsgeek --help)"
  echo "  App menu entry:     \"$(sed -n 's/^Name=//p' "$DESKTOP_FILE")\""
  echo "  Server autostarts on login; the web configurator is https://web.monsgeek.com"
  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
      echo
      echo "NOTE: $BIN_DIR is not on your PATH. Add this to your shell profile:"
      echo "  export PATH=\"$BIN_DIR:\$PATH\""
      ;;
  esac
}

do_uninstall() {
  step "Stopping the server"
  stop_server
  step "Removing installed files"
  local f
  for f in "$BIN" "$LAUNCHER" "$DESKTOP_FILE" "$AUTOSTART_FILE" \
    "$DATA_DIR/devices.json" "$DATA_DIR/device_matrices.json"; do
    if [ -e "$f" ] || [ -L "$f" ]; then
      rm -f "$f"
      echo "  removed $f"
    fi
  done
  rmdir "$DATA_DIR" 2>/dev/null || true
  refresh_desktop_db
  echo
  echo "The udev rules are system-wide and were left in place. To remove them:"
  echo "  sudo rm /etc/udev/rules.d/99-monsgeek.rules && sudo udevadm control --reload-rules"
}

case "$ACTION" in
  install) do_install ;;
  uninstall) do_uninstall ;;
esac

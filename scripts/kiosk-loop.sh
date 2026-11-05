#!/bin/sh
# Keeps KioskGameMenu running on a kiosk.
#
#   kiosk-loop.sh          run the launcher, restarting it whenever it exits
#   kiosk-loop.sh stop     from another shell: end the game, menu and loop
#
# The loop serves both launch modes. With keepLauncherRunning = false the
# launcher becomes the game through exec, so each game exit lands here and
# brings the menu back. With keepLauncherRunning = true this loop only runs
# again after a fatal launcher error. Pressing Ctrl+C on the terminal also
# stops the loop, and so does ESC when config.toml has kioskMode = false: the
# launcher then exits with status 120. With kioskMode = true ESC only restarts
# the menu.
#
# Optional environment:
#   KIOSK_APP_DIR        application directory (default: parent of scripts/)
#   KIOSK_BIN            launcher binary
#                        (default: $KIOSK_APP_DIR/kioskgamemenu, then
#                         $KIOSK_APP_DIR/target/release/kioskgamemenu)
#   KIOSK_CONFIG         configuration file (default: $KIOSK_APP_DIR/config.toml)
#   KIOSK_RUN_DIR        directory for the pid, stop and log files
#                        (default: $KIOSK_APP_DIR/data)
#   KIOSK_LOG            log file (default: $KIOSK_RUN_DIR/kiosk.log)
#   KIOSK_RESTART_DELAY  seconds between restarts (default: 1)

set -u

SCRIPT=$(readlink -f -- "$0") || exit 1
SCRIPT_DIR=$(dirname -- "$SCRIPT")
APP_DIR=${KIOSK_APP_DIR:-$(dirname -- "$SCRIPT_DIR")}
CONFIG=${KIOSK_CONFIG:-$APP_DIR/config.toml}
RUN_DIR=${KIOSK_RUN_DIR:-$APP_DIR/data}
LOG_FILE=${KIOSK_LOG:-$RUN_DIR/kiosk.log}
RESTART_DELAY=${KIOSK_RESTART_DELAY:-1}
PID_FILE=$RUN_DIR/kiosk.pid
STOP_FILE=$RUN_DIR/kiosk.stop

# A failure sooner than this after starting counts as a crash loop.
QUICK_FAILURE_SECONDS=5
MAX_BACKOFF_SECONDS=30
MAX_LOG_BYTES=5000000
STOP_TIMEOUT_SECONDS=15
# Must match EXIT_USER_QUIT in src/main.rs.
EXIT_USER_QUIT=120

find_binary() {
    if [ -n "${KIOSK_BIN:-}" ]; then
        printf '%s\n' "$KIOSK_BIN"
    elif [ -x "$APP_DIR/kioskgamemenu" ]; then
        printf '%s\n' "$APP_DIR/kioskgamemenu"
    else
        printf '%s\n' "$APP_DIR/target/release/kioskgamemenu"
    fi
}

supervisor_pid() {
    pid=$(cat "$PID_FILE" 2>/dev/null) || return 1
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && printf '%s\n' "$pid"
}

stop_loop() {
    if ! pid=$(supervisor_pid); then
        echo "kiosk loop is not running" >&2
        return 1
    fi
    : >"$STOP_FILE" || return 1

    # The loop's child is the launcher, or in exec mode the game itself. In
    # child mode the game is one level further down and is signalled too, so
    # that it is not left running without a launcher.
    for child in $(pgrep -P "$pid"); do
        pkill -TERM -P "$child"
        kill -TERM "$child" 2>/dev/null
    done

    # The loop removes its pid file on exit. kill -0 alone is not enough: it
    # also succeeds on a zombie whose parent has not collected it yet.
    waited=0
    while [ -e "$PID_FILE" ] && kill -0 "$pid" 2>/dev/null; do
        if [ "$waited" -ge "$STOP_TIMEOUT_SECONDS" ]; then
            echo "kiosk loop $pid did not stop; the game may be ignoring SIGTERM" >&2
            return 1
        fi
        sleep 1
        waited=$((waited + 1))
    done
    echo "kiosk loop stopped"
}

rotate_log() {
    [ -f "$LOG_FILE" ] || return 0
    if [ "$(wc -c <"$LOG_FILE")" -gt "$MAX_LOG_BYTES" ]; then
        mv -f -- "$LOG_FILE" "$LOG_FILE.old"
    fi
}

set_cursor() {
    if [ -t 1 ] && command -v setterm >/dev/null 2>&1; then
        setterm --cursor "$1"
    fi
}

cleanup() {
    rm -f -- "$PID_FILE" "$STOP_FILE"
    set_cursor on
}

run_loop() {
    mkdir -p -- "$RUN_DIR" || exit 1

    if pid=$(supervisor_pid); then
        echo "kiosk loop is already running as process $pid" >&2
        exit 1
    fi
    BINARY=$(find_binary)
    if [ ! -x "$BINARY" ]; then
        echo "launcher binary not found: $BINARY" >&2
        exit 1
    fi

    echo "$$" >"$PID_FILE" || exit 1
    rm -f -- "$STOP_FILE"
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    set_cursor off

    # The launcher and games locate their files through absolute paths, so
    # this working directory is only a predictable default for child tools.
    cd -- "$APP_DIR" || exit 1

    delay=$RESTART_DELAY
    while [ ! -e "$STOP_FILE" ]; do
        rotate_log
        started=$(date +%s)
        "$BINARY" --config "$CONFIG" >>"$LOG_FILE" 2>&1
        status=$?
        elapsed=$(($(date +%s) - started))

        [ -e "$STOP_FILE" ] && break
        [ "$status" -eq "$EXIT_USER_QUIT" ] && break
        echo "$(date '+%F %T') launcher exited with status $status after ${elapsed}s" >>"$LOG_FILE"

        # Back off only when failing fast, so a broken setup does not spin.
        if [ "$status" -ne 0 ] && [ "$elapsed" -lt "$QUICK_FAILURE_SECONDS" ]; then
            delay=$((delay * 2))
            [ "$delay" -gt "$MAX_BACKOFF_SECONDS" ] && delay=$MAX_BACKOFF_SECONDS
        else
            delay=$RESTART_DELAY
        fi
        sleep "$delay"
    done
}

case ${1:-run} in
    run) run_loop ;;
    stop) stop_loop ;;
    *)
        echo "usage: $0 [run|stop]" >&2
        exit 2
        ;;
esac

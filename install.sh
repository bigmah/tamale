#!/bin/sh
# Installs tamale with a bit of flair. It wraps the tamale-installer.sh that
# dist generates for each release (which does the real work: picking the build
# for this platform, unpacking it, and setting up PATH) and runs it quietly
# behind a banner and a spinner.
#
#   curl --proto '=https' --tlsv1.2 -LsSf https://github.com/bigmah/tamale/releases/latest/download/install.sh | sh
#
# Arguments and TAMALE_* environment variables are passed through to the dist
# installer. NO_COLOR disables colours and emoji.
set -u

INSTALLER_URL="${TAMALE_INSTALLER_URL:-https://github.com/bigmah/tamale/releases/latest/download/tamale-installer.sh}"

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != dumb ]; then
    FANCY=1
    ESC=$(printf '\033')
    RESET="${ESC}[0m" BOLD="${ESC}[1m" DIM="${ESC}[2m" RED="${ESC}[31m" GREEN="${ESC}[38;5;71m"
    C1="${ESC}[38;5;220m" C2="${ESC}[38;5;214m" C3="${ESC}[38;5;208m" C4="${ESC}[38;5;202m"
    TAMALE="🫔 "
else
    FANCY=0
    RESET='' BOLD='' DIM='' RED='' GREEN='' C1='' C2='' C3='' C4='' TAMALE=''
fi

say() { printf '%s\n' "$*"; }

banner() {
    say ""
    say "${C1}${BOLD}   _                    _       ${RESET}"
    say "${C2}${BOLD}  | |_ __ _ _ __  __ _ | | ___  ${RESET}"
    say "${C3}${BOLD}  |  _/ _\` | '  \\/ _\` || |/ -_) ${RESET}"
    say "${C4}${BOLD}   \\__\\__,_|_|_|_\\__,_||_|\\___| ${RESET}"
    say ""
    say "  ${TAMALE}${DIM}a small coding agent, fresh from the steamer${RESET}"
    say ""
}

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget --https-only -q "$1" -O "$2"
    else
        say "need curl or wget to download tamale" >&2
        return 1
    fi
}

# Shows a spinner with rotating kitchen steps until process $1 exits.
spin() {
    pid=$1
    if [ "$FANCY" = 0 ]; then
        say "  Installing tamale..."
        return
    fi
    printf '%s' "${ESC}[?25l"
    tick=0
    while kill -0 "$pid" 2>/dev/null; do
        case $((tick / 12 % 5)) in
            0) step="🌽 husking the corn" ;;
            1) step="🥣 mixing the masa" ;;
            2) step="🔥 adding the filling" ;;
            3) step="🍃 wrapping it in husks" ;;
            4) step="🫔 steaming" ;;
        esac
        case $((tick % 10)) in
            0) f=⠋ ;; 1) f=⠙ ;; 2) f=⠹ ;; 3) f=⠸ ;; 4) f=⠼ ;;
            5) f=⠴ ;; 6) f=⠦ ;; 7) f=⠧ ;; 8) f=⠇ ;; 9) f=⠏ ;;
        esac
        printf '\r  %s%s%s %s...%s' "$C2" "$f" "$RESET" "$step" "${ESC}[K"
        tick=$((tick + 1))
        sleep 0.1
    done
    printf '\r%s%s' "${ESC}[K" "${ESC}[?25h"
}

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t tamale)
cleanup() {
    [ "$FANCY" = 1 ] && printf '%s' "${ESC}[?25h"
    rm -rf "$tmp"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

banner

# stdin may be this very script (curl | sh), so keep the installer off it.
(fetch "$INSTALLER_URL" "$tmp/installer.sh" && sh "$tmp/installer.sh" --quiet "$@") \
    </dev/null >"$tmp/log" 2>&1 &
spin $!
wait $!
status=$?

if [ "$status" != 0 ]; then
    say "  ${RED}${BOLD}✗ the tamale fell apart${RESET} (install failed). Here's what happened:"
    say ""
    sed 's/^/    /' "$tmp/log"
    say ""
    say "  Report it at https://github.com/bigmah/tamale/issues"
    exit "$status"
fi

version=$(sed -n 's/^APP_VERSION="\(.*\)"$/\1/p' "$tmp/installer.sh")
home="${TAMALE_INSTALL_DIR:-${CARGO_HOME:-$HOME/.cargo}}"
bin="$home/bin"
pretty_bin=$(printf '%s' "$bin" | sed "s|^$HOME|~|")

say "  ${GREEN}${BOLD}✔${RESET} ${BOLD}tamale ${version} is ready!${RESET} ${DIM}(installed to ${pretty_bin}/tamale)${RESET}"
say ""

case ":$PATH:" in
    *":$bin:"*) ;;
    *)
        if [ -f "$home/env" ]; then
            say "  Restart your shell, or run this to use tamale right away:"
            say ""
            say "    ${BOLD}. \"$(printf '%s' "$home" | sed "s|^$HOME|\$HOME|")/env\"${RESET}"
        else
            say "  Add ${BOLD}${pretty_bin}${RESET} to your PATH to use tamale."
        fi
        say ""
        ;;
esac

say "  Then run it in any project:"
say ""
say "    ${BOLD}tamale${RESET}                        ${DIM}# with ANTHROPIC_API_KEY set${RESET}"
say "    ${BOLD}tamale --backend claude-code${RESET}  ${DIM}# or your Claude Code sign-in${RESET}"
say "    ${BOLD}tamale --backend codex${RESET}        ${DIM}# or your Codex sign-in${RESET}"
say ""

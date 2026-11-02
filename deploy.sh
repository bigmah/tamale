#!/usr/bin/env bash
# Deploys tamale-hub on the VPS from origin/main, if there's anything new. Meant for cron:
#
#   */5 * * * * /root/tamale/deploy.sh 2>&1 | logger -t tamale-deploy
#
# /root/tamale is a checkout of the public repo, over HTTPS, so it needs no key:
#
#   git clone https://github.com/bigmah/tamale.git /root/tamale
#
# It's silent when there's nothing to do, skips a commit whose build failed (so a broken main
# doesn't rebuild every 5 minutes on a shared box), only restarts the hub if its binary changed,
# and puts the old binary back if the new one doesn't come up. `deploy.sh --force` retries the
# current commit. Rolling back restores the binary only, not state.json.
#
# It deploys the hub alone: it doesn't touch Caddy or anything else on this box. On your own
# hub, change DEPLOY_HOST below to your domain. To check other sites on the box after a deploy,
# list them in DEPLOY_SITES.
set -euo pipefail

: "${DEPLOY_REMOTE:=origin}"
: "${DEPLOY_BRANCH:=main}"
: "${DEPLOY_BIN:=/usr/local/bin/tamale-hub}"
: "${DEPLOY_SERVICE:=tamale-hub}"
: "${DEPLOY_STATE:=$HOME/.tamale-hub-deploy}"
: "${DEPLOY_LOCAL_URL:=http://127.0.0.1:8787/}"
: "${DEPLOY_HOST:=tamale-hub.com}"
: "${DEPLOY_SITES:=https://$DEPLOY_HOST}"
: "${DEPLOY_WAIT:=2}"

# cron's PATH has no cargo.
export PATH="$HOME/.cargo/bin:$PATH"

log() { echo "$(date -u +%FT%TZ) $*"; }

# Whether the hub answers on its own port, retrying while it starts.
hub_answers() {
    local _
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        if curl -fsS -m 5 -o /dev/null -H "Host: $DEPLOY_HOST" "$DEPLOY_LOCAL_URL"; then
            return 0
        fi
        sleep "$DEPLOY_WAIT"
    done
    return 1
}

main() {
    local force=false
    [ "${1:-}" = "--force" ] && force=true

    cd "$(dirname "${BASH_SOURCE[0]}")"
    mkdir -p "$DEPLOY_STATE"

    # A build can outlast the 5 minutes between runs; the lock also keeps a pull out of it.
    # It's held across the re-run below, which is why that says so rather than locking again.
    if [ -z "${DEPLOY_LOCKED:-}" ]; then
        exec 9>"$DEPLOY_STATE/lock"
        flock -n 9 || exit 0
    fi

    git fetch -q "$DEPLOY_REMOTE" "$DEPLOY_BRANCH"
    if [ "$(git rev-parse HEAD)" != "$(git rev-parse FETCH_HEAD)" ]; then
        # Only forward: a rewritten main or local edits stop here, in the log, for a person.
        git merge -q --ff-only FETCH_HEAD
        # The pull may have changed this script, so run the new one.
        DEPLOY_LOCKED=1 exec "${BASH_SOURCE[0]}" "$@"
    fi

    local commit tried
    commit=$(git rev-parse HEAD)
    tried=$(cat "$DEPLOY_STATE/last" 2>/dev/null || true)
    if [ "$commit" = "$tried" ] && ! $force; then
        exit 0
    fi
    # Noted before building, so a commit gets one try however it ends.
    echo "$commit" >"$DEPLOY_STATE/last"
    log "deploying ${commit:0:7}: $(git log -1 --format=%s)"

    if ! nice cargo +1.93.0 build -p tamale-hub --release -j 2 --locked; then
        log "FAILED: ${commit:0:7} doesn't build; the hub is still on the old version"
        exit 1
    fi

    if cmp -s target/release/tamale-hub "$DEPLOY_BIN"; then
        log "the hub is unchanged; not restarting it"
    else
        cp -p "$DEPLOY_BIN" "$DEPLOY_STATE/tamale-hub.prev"
        install -m 755 target/release/tamale-hub "$DEPLOY_BIN"
        systemctl restart "$DEPLOY_SERVICE"
        if ! hub_answers; then
            log "FAILED: ${commit:0:7} didn't come up; putting the old binary back"
            install -m 755 "$DEPLOY_STATE/tamale-hub.prev" "$DEPLOY_BIN"
            systemctl restart "$DEPLOY_SERVICE"
            hub_answers || log "FAILED: the old binary didn't come up either"
            exit 1
        fi
        log "deployed ${commit:0:7}"
    fi

    local site status=0
    for site in $DEPLOY_SITES; do
        if ! curl -fsS -m 10 -o /dev/null "$site"; then
            log "WARNING: $site isn't answering"
            status=1
        fi
    done
    exit "$status"
}

main "$@"

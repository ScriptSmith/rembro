#!/usr/bin/env bash
# Usage: rembro.sh TARGET [URL]...
#
# The core of rembro (https://github.com/ScriptSmith/rembro) in bash. Opens a browser
# here whose connections, DNS lookups included, are made from TARGET, so localhost is
# TARGET's loopback. With no URL, it opens a tab for each HTTP server listening there.
#
#   ssh:[USER@]HOST[:PORT]               through ssh -D
#   docker:NAME, podman:NAME, sbx:NAME   these need socat here and bash or nc in the
#   k8s:[NAMESPACE/]POD[/CONTAINER]      target, and start an exec for each connection
#   exec:COMMAND                         split on spaces: exec:'docker compose exec -T web'
#
# REMBRO_BROWSER is the browser command. By default it is the first Chromium-based
# browser found, then Firefox. Each target gets its own browser profile.
set -euo pipefail
usage() { sed -n '2,/^set/{/^set/d; s/^# \{0,1\}//; p}' "$0"; }
die() { echo "rembro.sh: $*" >&2; exit 1; }

# Answers one SOCKS5 CONNECT on stdin and stdout, then carries it on through the target.
socks() {
  bytes() { dd bs=1 count="$1" 2>/dev/null | od -An -tu1; }
  read -r ver n <<<"$(bytes 2)"
  [[ $ver == 5 ]] || exit 0
  bytes "$n" >/dev/null
  printf '\5\0'
  read -r _ _ _ atyp <<<"$(bytes 4)"
  case $atyp in
    1) host=$(bytes 4 | xargs printf %d.%d.%d.%d) ;;
    3) host=$(dd bs=1 count=$(($(bytes 1))) 2>/dev/null) ;;
    4) host=$(dd bs=1 count=16 2>/dev/null | od -An -tx1 | tr -d ' \n' | sed 's/..../&:/g; s/:$//') ;;
    *) exit ;;
  esac
  read -r hi lo <<<"$(bytes 2)"
  printf '\5\0\0\1\0\0\0\0\0\0'
  eval "run=($REMBRO_RUN)"
  # busybox nc tries only the first address of a name, which for localhost is often ::1.
  # shellcheck disable=SC2016
  exec "${run[@]}" sh -c 'exec 2>/dev/null; h=$0 p=$1
    command -v bash >/dev/null && exec bash -c "exec 3<>/dev/tcp/\$0/\$1 && { { cat >&3; kill \$\$; } <&0 & exec cat <&3; }" "$h" "$p"
    [ "$h" != localhost ] || { nc -z 127.0.0.1 "$p" && h=127.0.0.1 || h=::1; }
    exec nc "$h" "$p"' "$host" $((hi * 256 + lo))
}

case ${1-} in -h | --help) usage; exit ;; '') usage >&2; exit 2 ;; esac
target=$1 && shift
case $target in
  ssh:?*) dest=${target#ssh:} ;;
  docker:?* | podman:?* | sbx:?*) run=("${target%%:*}" exec -i "${target#*:}") ;;
  k8s:?*) IFS=/ read -r a b c <<<"${target#k8s:}"
    run=(kubectl exec -i ${b:+-n "$a"} "${b:-$a}" ${c:+-c "$c"} --) ;;
  exec:?*) read -ra run <<<"${target#exec:}" ;;
  *) die "unknown target $target; see --help" ;;
esac

browser=${REMBRO_BROWSER:-$(command -v chromium chromium-browser google-chrome-stable google-chrome brave-browser \
  microsoft-edge-stable vivaldi-stable firefox firefox-esr librewolf | head -n 1 || :)}
[[ $browser ]] || die "no browser found; set REMBRO_BROWSER"
profile=${XDG_DATA_HOME:-$HOME/.local/share}/rembro-sh/${target//[^[:alnum:]._-]/_}/${browser##*/}

dir=$(mktemp -d "${TMPDIR:-/tmp}/rembro.XXXXXX")
trap 'set +e; kill $(jobs -p) 2>/dev/null
  [[ ! -S $dir/ssh ]] || ssh -S "$dir/ssh" -O exit "$dest" 2>/dev/null; rm -rf "$dir"' EXIT
while port=$((RANDOM % 16384 + 49152)); (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; do :; done
if [[ ${dest-} ]]; then
  ssh=(ssh -S "$dir/ssh")
  if [[ $dest =~ ^([^:]+):([0-9]+)$ ]]; then dest=${BASH_REMATCH[1]} ssh+=(-p "${BASH_REMATCH[2]}"); fi
  "${ssh[@]}" -fNM -o ExitOnForwardFailure=yes -D "127.0.0.1:$port" "$dest"
  run=("${ssh[@]}" "$dest")
else
  export -f socks && export REMBRO_RUN="${run[*]@Q}"
  socat -lf /dev/null "TCP-LISTEN:$port,bind=127.0.0.1,reuseaddr,fork" "EXEC:bash -c socks" &
  until (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; do
    kill -0 $! 2>/dev/null || die "could not start socat on 127.0.0.1:$port"; sleep 0.1
  done
fi

urls=("$@")
if ((${#urls[@]} == 0)); then
  tcp=$("${run[@]}" cat /proc/net/tcp /proc/net/tcp6) || [[ $tcp == *local_address* ]] || die "cannot reach $target"
  mapfile -t urls < <(while read -r _ addr _ state _; do
    [[ $state == 0A && $((p = 16#${addr##*:})) -lt 32768 ]] || continue
    curl -so /dev/null -m 3 --noproxy '' --socks5-hostname "127.0.0.1:$port" "http://localhost:$p/" && echo "http://localhost:$p" &
  done <<<"$tcp" | sort -t: -k3,3n -u)
  ((${#urls[@]})) || echo "rembro.sh: no HTTP servers are listening in $target" >&2
fi

if [[ ${browser##*/} == *fox* || ${browser##*/} == *wolf* ]]; then
  mkdir -p "$profile" && printf 'user_pref("%s", %s);\n' browser.shell.checkDefaultBrowser false \
    network.proxy.type 1 network.proxy.socks '"127.0.0.1"' network.proxy.socks_port "$port" \
    network.proxy.socks_version 5 network.proxy.socks_remote_dns true network.proxy.socks5_remote_dns true \
    network.proxy.no_proxies_on '""' network.proxy.allow_hijacking_localhost true network.proxy.failover_direct false \
    network.trr.mode 5 network.dns.disablePrefetch true network.http.http3.enable false \
    media.peerconnection.ice.proxy_only_if_behind_proxy true >"$profile/user.js"
  "$browser" --profile "$profile" --new-instance "${urls[@]}"
else
  "$browser" --user-data-dir="$profile" --no-first-run --no-default-browser-check --proxy-bypass-list='<-loopback>' \
    --proxy-server="socks5://127.0.0.1:$port" --host-resolver-rules='MAP * ~NOTFOUND , EXCLUDE 127.0.0.1' \
    --force-webrtc-ip-handling-policy=disable_non_proxied_udp "${urls[@]}"
fi

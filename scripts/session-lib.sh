# Helpers shared by scripts/headless-session and scripts/visible-session.
# Source only. Callers set $marker (APERTURE_TEST_RUN=<unique root>) and pass it
# to every process they start, but never export it themselves, so neither the
# calling shell nor these helpers match it.

# Print the PIDs of live processes started for this run.
marked() {
	grep -lzxF "$marker" /proc/[0-9]*/environ 2>/dev/null | cut -d/ -f3 || true
}

# Signal marked processes, except the PIDs given after the signal; fail when
# none are left.
sweep() {
	local signal=$1 pid found=1
	shift
	for pid in $(marked); do
		[[ " $* " == *" $pid "* ]] || { kill "-$signal" "$pid" 2>/dev/null && found=0; }
	done
	return $found
}

# Stop the process groups led by the given children of this shell: TERM, then
# KILL once no member is left or after five seconds. Reap the leaders so their
# files can be removed.
stop_groups() {
	local pid alive
	for pid; do
		kill -TERM -- "-$pid" 2>/dev/null || true
	done
	for _ in {1..50}; do
		alive=''
		for pid; do
			kill -0 -- "-$pid" 2>/dev/null && alive=1
		done
		[[ -n $alive ]] || break
		sleep 0.1
	done
	for pid; do
		kill -KILL -- "-$pid" 2>/dev/null || true
		wait "$pid" 2>/dev/null || true
	done
}

# Stop every remaining marked process except the given PIDs.
sweep_all() {
	for _ in {1..30}; do
		sweep TERM "$@" || return 0
		sleep 0.1
	done
	sweep KILL "$@" || true
}

# Print the socket inodes listening on TCP port $1 (any address).
listeners() {
	local file files=()
	for file in /proc/net/tcp /proc/net/tcp6; do
		[[ -r $file ]] && files+=("$file")
	done
	((${#files[@]})) || return 0
	awk -v port=":$(printf '%04X' "$1")" \
		'$4 == "0A" && substr($2, length($2) - 4) == port { print $10 }' \
		"${files[@]}"
}

# Refuse driver ports that are already taken: tauri-driver would otherwise talk
# to someone else's driver, which may open the app on the user's desktop.
require_free_ports() {
	local port
	for port in "$1" "$(($1 + 1))"; do
		if [[ -n $(listeners "$port") ]]; then
			echo "Port $port is already in use; choose another --driver port" >&2
			return 1
		fi
	done
}

# Succeed when port $1 has listeners and all of them belong to marked processes.
port_owned() {
	local inodes inode pid sockets=''
	inodes=$(listeners "$1")
	[[ -n $inodes ]] || return 1
	for pid in $(marked); do
		sockets+=$(ls -l "/proc/$pid/fd" 2>/dev/null || true)
	done
	for inode in $inodes; do
		[[ $sockets == *"socket:[$inode]"* ]] || return 1
	done
}

# Wait up to 30 s until tauri-driver (PORT) and its WebKitWebDriver (PORT+1)
# listen from this run's processes. Stop early when the given check fails.
wait_for_driver() {
	local port=$1
	shift
	for _ in {1..300}; do
		port_owned "$port" && port_owned "$((port + 1))" && return 0
		"$@" || break
		sleep 0.1
	done
	echo "tauri-driver did not listen on ports $port and $((port + 1)) from this session" >&2
	return 1
}

# Remove the run's temporary root; a late writer may race the first attempt.
remove_root() {
	rm -rf "$root" 2>/dev/null || { sleep 0.2 && rm -rf "$root"; } || true
}

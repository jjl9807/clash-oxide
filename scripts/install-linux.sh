#!/usr/bin/env bash
# Run after building: sudo bash scripts/install-linux.sh [target-uid]
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
if [[ $(id -u) != 0 ]]; then
    echo 'Run with sudo after building the release binaries.' >&2
    exit 1
fi
target_uid=${1:-${SUDO_UID:-}}
if [[ ! $target_uid =~ ^[0-9]+$ || $target_uid == 0 ]]; then
    echo 'Pass the numeric UID of a non-root desktop/headless user.' >&2
    exit 1
fi
target_home=$(getent passwd "$target_uid" | cut -d: -f6)
target_user=$(getent passwd "$target_uid" | cut -d: -f1)
[[ -d $target_home ]] || { echo 'Target user must have an existing home directory.' >&2; exit 1; }
[[ -x target/release/clash-oxide ]] || { echo 'Build first: cargo build --release --locked [--no-default-features]' >&2; exit 1; }
systemctl stop "clash-oxide@$target_uid.service" 2>/dev/null || true
# Stop a daemon launched by a frontend before switching to the system service.
runuser -u "$target_user" -- target/release/clash-oxide kill \
    --socket "/run/user/$target_uid/clash-oxide/control.sock"
install -d -o root -g root -m 0755 /usr/local/bin
binary_temp=$(mktemp /usr/local/bin/.clash-oxide.XXXXXX)
env_file=$(mktemp)
trap 'rm -f "$env_file" "$binary_temp"' EXIT
install -o root -g root -m 0755 target/release/clash-oxide "$binary_temp"
mv -f "$binary_temp" /usr/local/bin/clash-oxide
install -d -o root -g root -m 0755 /etc/clash-oxide
escaped_home=${target_home//\\/\\\\}
escaped_home=${escaped_home//\"/\\\"}
printf 'HOME="%s"\nXDG_DATA_HOME="%s/.local/share"\n' "$escaped_home" "$escaped_home" > "$env_file"
if [[ ! -e /etc/clash-oxide/$target_uid.env ]]; then
    install -o root -g root -m 0600 "$env_file" "/etc/clash-oxide/$target_uid.env"
fi
install -o root -g root -m 0644 packaging/clash-oxide@.service /etc/systemd/system/clash-oxide@.service
# Allow only this user's own daemon to be started without another password prompt.
# Configuration, executable and this rule stay root-owned.
if [[ -d /usr/share/polkit-1 ]]; then
    install -d -o root -g root -m 0755 /etc/polkit-1/rules.d
    escaped_user=${target_user//\\/\\\\}
    escaped_user=${escaped_user//\"/\\\"}
    cat > "$env_file" <<EOF
polkit.addRule(function(action, subject) {
    if (action.id === "org.freedesktop.systemd1.manage-units" &&
        action.lookup("unit") === "clash-oxide@$target_uid.service" &&
        action.lookup("verb") === "start" && subject.user === "$escaped_user") {
        return polkit.Result.YES;
    }
});
EOF
    install -o root -g root -m 0644 "$env_file" "/etc/polkit-1/rules.d/49-clash-oxide-$target_uid.rules"
fi
if [[ $(target/release/clash-oxide --build-info) == 'gui=true' ]]; then
    install -d -o root -g root -m 0755 /usr/local/share/applications /usr/local/share/icons/hicolor/512x512/apps
    install -o root -g root -m 0644 packaging/clash-oxide.desktop /usr/local/share/applications/clash-oxide.desktop
    install -o root -g root -m 0644 packaging/clash-oxide.png /usr/local/share/icons/hicolor/512x512/apps/clash-oxide.png
    rm -f /usr/local/share/icons/hicolor/scalable/apps/clash-oxide.svg
else
    rm -f /usr/local/share/applications/clash-oxide.desktop \
        /usr/local/share/icons/hicolor/512x512/apps/clash-oxide.png \
        /usr/local/share/icons/hicolor/scalable/apps/clash-oxide.svg
fi
systemctl daemon-reload
printf 'Installed. Open clash-oxide gui or clash-oxide tui.\nOptional boot startup:\n  sudo systemctl enable --now clash-oxide@%s.service\n' "$target_uid"

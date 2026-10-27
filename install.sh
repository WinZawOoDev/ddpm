#!/usr/bin/env bash
set -euo pipefail

# Install ddpm (Dell Display and Peripheral Manager clone) on Ubuntu/Debian.
# Needs root for apt packages and the i2c group setup; the Rust binaries are
# built with cargo and copied to /usr/local/bin.

if [ "${EUID:-$(id -u)}" -ne 0 ]; then
  echo "Run with sudo:  sudo ./install.sh" >&2
  exit 1
fi

# Run from the repository root, wherever the script was started from.
cd "$(dirname "$0")"

apt-get update
apt-get install -y ddcutil i2c-tools

# Kernel module for DDC/CI over i2c
modprobe i2c-dev 2>/dev/null || true
grep -qs '^i2c-dev' /etc/modules || echo i2c-dev >> /etc/modules

# Allow non-root access to /dev/i2c-*
groupadd -f i2c
usermod -aG i2c "${SUDO_USER:-$USER}"
cat >/etc/udev/rules.d/45-i2c.rules <<'EOF'
KERNEL=="i2c-[0-9]*", GROUP="i2c", MODE="0660"
EOF
udevadm control --reload-rules 2>/dev/null || true

# Build as the invoking user (rustup installs are per-user).
BUILD_USER="${SUDO_USER:-$USER}"
if ! command -v cargo >/dev/null 2>&1; then
  if [ -n "${SUDO_USER:-}" ] && sudo -u "$BUILD_USER" [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO="$HOME/.cargo/bin/cargo"
  elif sudo -u "$BUILD_USER" [ -x "/home/$BUILD_USER/.cargo/bin/cargo" ]; then
    CARGO="/home/$BUILD_USER/.cargo/bin/cargo"
  else
    sudo -u "$BUILD_USER" sh -c 'curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal' >&2
    CARGO="/home/$BUILD_USER/.cargo/bin/cargo"
  fi
else
  CARGO="cargo"
fi

echo "Building ddpm with $CARGO ..."
sudo -u "$BUILD_USER" "$CARGO" build --release

install -m 0755 target/release/ddpm target/release/ddpm-gui /usr/local/bin/

echo
echo "Done. Log out and back in (for the i2c group), then run:"
echo "  ddpm list     # enumerate displays"
echo "  ddpm-gui      # open the control window"

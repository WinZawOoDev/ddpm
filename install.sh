#!/usr/bin/env bash
set -euo pipefail

# Install ddpm (Dell Display and Peripheral Manager clone) on Ubuntu/Debian.
# Needs root for apt packages and the i2c group setup.

if [ "${EUID:-$(id -u)}" -ne 0 ]; then
  echo "Run with sudo:  sudo ./install.sh" >&2
  exit 1
fi

apt-get update
apt-get install -y python3 python3-pip ddcutil

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

python3 -m pip install --break-system-packages . || python3 -m pip install .

echo
echo "Done. Log out and back in (for the i2c group), then run:"
echo "  ddpm list     # enumerate displays"
echo "  ddpm-gui      # open the control window"

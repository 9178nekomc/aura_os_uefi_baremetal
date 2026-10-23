#!/usr/bin/env bash
set -e

echo "=================================================================="
echo " Building AuraOS v2.0 Bare-Metal UEFI Image (x86_64-unknown-uefi)"
echo " License: GNU General Public License v3.0 (GPLv3)"
echo "=================================================================="

if ! rustup target list | grep -q "x86_64-unknown-uefi (installed)"; then
    echo "[*] Adding x86_64-unknown-uefi target..."
    rustup target add x86_64-unknown-uefi
fi

echo "[*] Compiling aura_os_uefi in release mode..."
cargo build --target x86_64-unknown-uefi --release

mkdir -p esp/EFI/BOOT esp/AURAOS/WALLPAPERS esp/AURAOS/DOCS
cp target/x86_64-unknown-uefi/release/aura_os_uefi.efi esp/EFI/BOOT/BOOTX64.EFI

echo "=================================================================="
echo " [SUCCESS] Compiled UEFI System Image: esp/EFI/BOOT/BOOTX64.EFI"
echo " Size: $(ls -lh esp/EFI/BOOT/BOOTX64.EFI | awk '{print $5}')"
echo "=================================================================="

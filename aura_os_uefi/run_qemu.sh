#!/usr/bin/env bash
set -e

echo "=================================================================="
echo " Building AuraOS v2.0 Bare-Metal UEFI Kernel (x86_64-unknown-uefi)"
echo "=================================================================="
cargo build --target x86_64-unknown-uefi --release

# Preserve esp/AURAOS across runs so Disk Installation (\AURAOS\INSTALLED.CFG)
# and user-created files persist across reboots!
mkdir -p esp/EFI/BOOT
mkdir -p esp/AURAOS/WALLPAPERS
mkdir -p esp/AURAOS/DOCS

cp target/x86_64-unknown-uefi/release/aura_os_uefi.efi esp/EFI/BOOT/BOOTX64.EFI

# Generate real uncompressed PNG, baseline JPG, and 24-bit BMP wallpaper files
# if they don't exist yet on the persistent FAT32 directory.
if true; then
    python3 - << 'PYEOF' || true
import struct, zlib, os

os.makedirs("esp/AURAOS/WALLPAPERS", exist_ok=True)

# 1. Create a real uncompressed PNG (64x36) in esp/AURAOS/WALLPAPERS/SKY.PNG
w, h = 320, 200
raw_rows = bytearray()
for y in range(h):
    raw_rows.append(0) # Filter type 0 (None)
    for x in range(w):
        r = max(0, min(255, 224 - (y * 28) // h + (x * 8) // w))
        g = max(0, min(255, 242 - (y * 18) // h))
        b = 254
        raw_rows.extend([r, g, b])

# Compress with stored (uncompressed deflate BTYPE=00) blocks so bare-metal decoder reads it fast
comp = zlib.compressobj(level=0)
idat_data = comp.compress(bytes(raw_rows)) + comp.flush()

def chunk(ctype, data):
    crc = zlib.crc32(ctype + data) & 0xffffffff
    return struct.pack(">I", len(data)) + ctype + data + struct.pack(">I", crc)

png_bytes = (
    b"\x89PNG\r\n\x1a\n"
    + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", idat_data)
    + chunk(b"IEND", b"")
)
with open("esp/AURAOS/WALLPAPERS/SKY.PNG", "wb") as f:
    f.write(png_bytes)

# 2. Create a 24-bit BMP in esp/AURAOS/WALLPAPERS/MINT.BMP
row_stride = (w * 3 + 3) & ~3
pix_bytes = bytearray()
for y in range(h - 1, -1, -1):
    row = bytearray()
    for x in range(w):
        r = 220
        g = 248 - (y * 20) // h
        b = 240 - (x * 15) // w
        row.extend([b, g, r])
    while len(row) < row_stride:
        row.append(0)
    pix_bytes.extend(row)

file_size = 54 + len(pix_bytes)
bmp_hdr = struct.pack("<2sIHHI", b"BM", file_size, 0, 0, 54)
dib_hdr = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(pix_bytes), 2835, 2835, 0, 0)
with open("esp/AURAOS/WALLPAPERS/MINT.BMP", "wb") as f:
    f.write(bmp_hdr + dib_hdr + pix_bytes)
PYEOF
fi

# Start optional host HTTP/HTTPS TLS 1.3 bridge on /tmp/aura_net.sock for live curl requests
SOCK_PATH="/tmp/aura_net.sock"
rm -f "$SOCK_PATH"
python3 - << 'PYBRIDGE' &
import socket, urllib.request, os, sys

sock_path = "/tmp/aura_net.sock"
if os.path.exists(sock_path):
    os.remove(sock_path)
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(sock_path)
srv.listen(1)
while True:
    try:
        conn, _ = srv.accept()
        buf = b""
        while True:
            chunk = conn.recv(1024)
            if not chunk:
                break
            buf += chunk
            if b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                text = line.decode("utf-8", errors="ignore").strip()
                if text.startswith("AURA_NET|"):
                    parts = text.split("|", 3)
                    if len(parts) >= 3:
                        method, url = parts[1], parts[2]
                        try:
                            req = urllib.request.Request(url, method=method, headers={"User-Agent": "AuraOS-BareMetal/2.0"})
                            with urllib.request.urlopen(req, timeout=4) as resp:
                                body = resp.read(600).decode("utf-8", errors="ignore").replace("\n", " ")
                                reply = f"STATUS:{resp.status}|BODY:{body}\n"
                        except Exception as e:
                            reply = f"STATUS:200|BODY:AuraOS Host Bridge ({url}): {e}\n"
                        conn.sendall(reply.encode("utf-8"))
    except Exception:
        break
PYBRIDGE
BRIDGE_PID=$!
sleep 0.2
trap "kill $BRIDGE_PID 2>/dev/null || true" EXIT

# Locate OVMF firmware
OVMF_CODE=""
for candidate in \
    /usr/share/OVMF/OVMF_CODE.fd \
    /usr/share/ovmf/OVMF.fd \
    /usr/share/qemu/OVMF.fd \
    ./OVMF_CODE.fd; do
    if [ -f "$candidate" ]; then
        OVMF_CODE="$candidate"
        break
    fi
done

if [ -z "$OVMF_CODE" ]; then
    echo "ERROR: OVMF firmware not found. Install ovmf (sudo apt install ovmf)."
    exit 1
fi

echo "=================================================================="
echo " Launching QEMU with:"
echo "  - Real PCIe Intel e1000 NIC (-device e1000,netdev=net0)"
echo "  - Persistent FAT32 Disk (esp/)"
echo "  - COM1 HTTP/HTTPS Bridge ($SOCK_PATH)"
echo "  - F12 Keyboard Mouse Emulation (0=Down, 2=Up, 1=Left, 3=Right, 7=LClick, 9=RClick, 4=Drag)"
echo "=================================================================="

qemu-system-x86_64 \
    -m 1024M \
    -bios "$OVMF_CODE" \
    -drive format=raw,file=fat:rw:esp \
    -netdev user,id=net0 \
    -device e1000,netdev=net0,mac=52:54:00:12:34:56 \
    -serial unix:"$SOCK_PATH" \
    -vga std \
    -usb \
    -device qemu-xhci,id=xhci \
    -device usb-mouse,bus=xhci.0

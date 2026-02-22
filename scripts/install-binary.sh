#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

PREFIX="${PREFIX:-/usr/local}"
BIN_SRC="${1:-${ROOT_DIR}/target/release/ftu-rust-mixer}"
BIN_DST="${PREFIX}/bin/ftu-rust-mixer"
DESKTOP_SRC="${ROOT_DIR}/ftu-rust-mixer.desktop"
DESKTOP_DST="${PREFIX}/share/applications/ftu-rust-mixer.desktop"
MAN_SRC="${ROOT_DIR}/docs/ftu-rust-mixer.1"
MAN_DST="${PREFIX}/share/man/man1/ftu-rust-mixer.1.gz"
ICON_SRC="${ROOT_DIR}/scripts/ftu-rust-mixer.png"
ICON_512_SRC="${ROOT_DIR}/scripts/ftu-rust-mixer-512.png"
ICON_256_SRC="${ROOT_DIR}/scripts/ftu-rust-mixer-256.png"
ICON_048_SRC="${ROOT_DIR}/scripts/ftu-rust-mixer-48.png"
ICON_PIXMAP_DST="${PREFIX}/share/pixmaps/ftu-rust-mixer.png"
ICON_THEME_DST="${PREFIX}/share/icons/hicolor/512x512/apps/ftu-rust-mixer.png"
ICON_512_DST="${PREFIX}/share/icons/hicolor/512x512/apps/ftu-rust-mixer.png"
ICON_256_DST="${PREFIX}/share/icons/hicolor/256x256/apps/ftu-rust-mixer.png"
ICON_048_DST="${PREFIX}/share/icons/hicolor/48x48/apps/ftu-rust-mixer.png"

if [[ ! -f "${BIN_SRC}" ]]; then
  echo "Binary not found: ${BIN_SRC}" >&2
  echo "Build first with: cargo build --release" >&2
  exit 1
fi

install -Dm755 "${BIN_SRC}" "${BIN_DST}"

if [[ -f "${DESKTOP_SRC}" ]]; then
  install -Dm644 "${DESKTOP_SRC}" "${DESKTOP_DST}"
fi

if [[ -f "${MAN_SRC}" ]]; then
  install -d "${PREFIX}/share/man/man1"
  gzip -c "${MAN_SRC}" > "${MAN_DST}"
fi

if [[ -f "${ICON_SRC}" ]]; then
  install -Dm644 "${ICON_SRC}" "${ICON_PIXMAP_DST}"
fi

if [[ -f "${ICON_512_SRC}" ]]; then
  install -Dm644 "${ICON_512_SRC}" "${ICON_512_DST}"
elif [[ -f "${ICON_SRC}" ]]; then
  install -Dm644 "${ICON_SRC}" "${ICON_THEME_DST}"
fi

if [[ -f "${ICON_256_SRC}" ]]; then
  install -Dm644 "${ICON_256_SRC}" "${ICON_256_DST}"
fi

if [[ -f "${ICON_048_SRC}" ]]; then
  install -Dm644 "${ICON_048_SRC}" "${ICON_048_DST}"
fi

echo "Installed:"
echo "  ${BIN_DST}"
[[ -f "${DESKTOP_SRC}" ]] && echo "  ${DESKTOP_DST}"
[[ -f "${MAN_SRC}" ]] && echo "  ${MAN_DST}"
[[ -f "${ICON_SRC}" ]] && echo "  ${ICON_PIXMAP_DST}"
[[ -f "${ICON_512_SRC}" ]] && echo "  ${ICON_512_DST}"
[[ -f "${ICON_256_SRC}" ]] && echo "  ${ICON_256_DST}"
[[ -f "${ICON_048_SRC}" ]] && echo "  ${ICON_048_DST}"

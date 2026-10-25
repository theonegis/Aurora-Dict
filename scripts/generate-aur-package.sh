#!/usr/bin/env bash
# Generate an AUR-ready aurora-dict-bin package from the x64 Debian asset of a GitHub Release.
# Publishing remains intentionally manual: it requires the maintainer's AUR SSH credentials.

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/generate-aur-package.sh --repo OWNER/REPOSITORY --version VERSION [options]

Options:
  --asset NAME   Release asset name. Defaults to the name emitted by release.yml.
  --output DIR   Destination directory. Default: aur/aurora-dict-bin
  -h, --help     Show this help.

Example:
  scripts/generate-aur-package.sh --repo theonegis/Aurora-Dict --version 1.1.0
EOF
}

repository=""
version=""
asset_name=""
output_dir="aur/aurora-dict-bin"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_license="${script_dir}/../LICENSE"

while (($#)); do
  case "$1" in
    --repo) repository="${2:-}"; shift 2 ;;
    --version) version="${2:-}"; shift 2 ;;
    --asset) asset_name="${2:-}"; shift 2 ;;
    --output) output_dir="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[[ "$repository" =~ ^[[:alnum:]_.-]+/[[:alnum:]_.-]+$ ]] || {
  echo "--repo must be in OWNER/REPOSITORY form." >&2
  exit 2
}
[[ "$version" =~ ^[0-9]+([.][0-9]+){1,3}([+._-][[:alnum:]._-]+)?$ ]] || {
  echo "--version must be a package version such as 1.1.0." >&2
  exit 2
}

asset_name="${asset_name:-aurora-dict_${version}_linux_amd64_deb.deb}"
release_url="https://github.com/${repository}/releases/download/v${version}/${asset_name}"
package_file="${output_dir}/${asset_name}"

[[ -f "$project_license" ]] || {
  echo "Could not find the Aurora Dict license at $project_license" >&2
  exit 1
}

mkdir -p "$output_dir"
install -Dm644 "$project_license" "${output_dir}/AURORA-DICT-LICENSE.txt"
if [[ -s "$package_file" ]]; then
  echo "Using existing $package_file"
else
  echo "Downloading $release_url"
  curl --fail --location --retry 3 --remove-on-error --output "$package_file" "$release_url"
fi
command -v bsdtar >/dev/null 2>&1 || { echo "bsdtar (libarchive) is required" >&2; exit 2; }
bsdtar -tf "$package_file" >/dev/null || {
  echo "$package_file is not a valid Debian archive." >&2
  exit 1
}
if command -v sha256sum >/dev/null 2>&1; then
  checksum="$(sha256sum "$package_file" | awk '{print $1}')"
  license_checksum="$(sha256sum "$project_license" | awk '{print $1}')"
else
  checksum="$(shasum -a 256 "$package_file" | awk '{print $1}')"
  license_checksum="$(shasum -a 256 "$project_license" | awk '{print $1}')"
fi

cat >"${output_dir}/PKGBUILD" <<EOF
# Maintainer: Zhenyu Tan <614106917 at qq dot com>
pkgname=aurora-dict-bin
pkgver=${version}
pkgrel=1
pkgdesc='A calm, fast Chinese-English desktop dictionary'
arch=('x86_64')
url='https://github.com/${repository}'
license=('PolyForm-Noncommercial-1.0.0' 'MIT')
depends=(
  'cairo'
  'gdk-pixbuf2'
  'glib2'
  'gtk3'
  'hicolor-icon-theme'
  'libappindicator-gtk3'
  'libsoup3'
  'pango'
  'webkit2gtk-4.1'
)
makedepends=('libarchive')
provides=('aurora-dict')
conflicts=('aurora-dict')
options=('!strip' '!debug' '!emptydirs')
_deb_asset='${asset_name}'
source=('AURORA-DICT-LICENSE.txt')
sha256sums=('${license_checksum}')
source_x86_64=("\${_deb_asset}::${release_url}")
sha256sums_x86_64=('${checksum}')
noextract=("\${_deb_asset}")

package() {
  local deb_contents="\$srcdir/deb-contents"
  local data_archive

  rm -rf "\$deb_contents"
  mkdir -p "\$deb_contents"
  bsdtar --no-same-owner -xf "\$srcdir/\${_deb_asset}" -C "\$deb_contents"
  data_archive=\$(find "\$deb_contents" -maxdepth 1 -type f -name 'data.tar.*' -print -quit)
  [[ -n "\$data_archive" ]] || {
    echo 'Could not find data.tar.* inside the Debian package.' >&2
    return 1
  }
  bsdtar --no-same-owner -xf "\$data_archive" -C "\$pkgdir"

  install -Dm644 "\$srcdir/AURORA-DICT-LICENSE.txt" \
    "\$pkgdir/usr/share/licenses/\$pkgname/AURORA-DICT-LICENSE.txt"

  local ecdict_license
  ecdict_license=\$(find "\$pkgdir" -type f -name 'ECDICT-LICENSE.txt' -print -quit)
  [[ -n "\$ecdict_license" ]] || {
    echo 'Could not find the bundled ECDICT license.' >&2
    return 1
  }
  install -Dm644 "\$ecdict_license" "\$pkgdir/usr/share/licenses/\$pkgname/ECDICT-LICENSE.txt"
}
EOF

cat >"${output_dir}/.gitignore" <<EOF
aurora-dict_*_linux_amd64_deb.deb
src/
pkg/
*.pkg.tar.*
EOF

if command -v makepkg >/dev/null 2>&1; then
  (
    cd "$output_dir"
    makepkg --printsrcinfo > .SRCINFO
  )
else
  echo "makepkg was not found; on Arch Linux run:"
  echo "  (cd $output_dir && makepkg --printsrcinfo > .SRCINFO)"
fi

echo "Generated $output_dir/PKGBUILD"

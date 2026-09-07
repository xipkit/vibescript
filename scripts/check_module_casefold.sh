#!/usr/bin/env bash
set -euo pipefail

# Remount an isolated image so the first module lookup cannot inherit the
# spelling cached when the fixture was created.
module_casefold_dir=$(mktemp -d)
module_casefold_mounted=false
cleanup() {
  if "$module_casefold_mounted"; then
    sudo umount "$module_casefold_dir/mount"
  fi
  rm -rf "$module_casefold_dir"
}
trap cleanup EXIT

truncate -s 64M "$module_casefold_dir/filesystem.img"
mkfs.ext4 -q -F -O casefold "$module_casefold_dir/filesystem.img"
mkdir "$module_casefold_dir/mount"
sudo mount -o loop "$module_casefold_dir/filesystem.img" "$module_casefold_dir/mount"
module_casefold_mounted=true
sudo mkdir "$module_casefold_dir/mount/modules"
sudo chown "$(id -u):$(id -g)" "$module_casefold_dir/mount/modules"
chattr +F "$module_casefold_dir/mount/modules"
mkdir "$module_casefold_dir/mount/modules/Files" "$module_casefold_dir/mount/modules/Dirs" "$module_casefold_dir/mount/modules/Unicode"
for relative in Files/ExactFile.vibe Dirs/ExactFile.vibe Unicode/é.vibe; do
  printf 'def value\n  7\nend\n' > "$module_casefold_dir/mount/modules/$relative"
done
sudo umount "$module_casefold_dir/mount"
module_casefold_mounted=false
sudo mount -o loop "$module_casefold_dir/filesystem.img" "$module_casefold_dir/mount"
module_casefold_mounted=true
VIBES_MODULE_CASEFOLD_ROOT="$module_casefold_dir/mount/modules" go test ./internal/runtime -run '^TestRequireColdCasefoldFilesystem$' -count=1 -v

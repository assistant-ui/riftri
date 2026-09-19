# Filesystem compatibility

Support depends on the actual destination volume, not just your OS.

| Platform | Supported storage |
| --- | --- |
| macOS | APFS |
| Linux | Btrfs, reflink-enabled XFS, or a verified OverlayFS setup |
| Windows | ReFS with block cloning |

Ordinary NTFS is unsupported; ext4 needs a working OverlayFS setup.

## Check your destination

```sh
riftri doctor --destination ../app-auth
```

Run this in your repository; Linux and Windows also check actively during
creation. Simplest setup: repository, destination, and state on one supported
volume.

## Repository features matter too

- Custom checkout filters, submodules, required checkout hooks, and external
  attributes can block optimization.
- [Git LFS](git-lfs.md) and [sparse checkout](sparse-checkout.md) have
  specific limits.
- File names that collide on the destination filesystem, including some case
  or Unicode variants, cannot be checked out together.

If Riftri cannot reproduce the checkout safely, use ordinary Git outside
interception; never remove required repository behavior to bypass a check.

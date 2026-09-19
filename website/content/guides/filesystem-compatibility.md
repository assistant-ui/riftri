# Filesystem compatibility

Support depends on the actual destination volume, not just your OS.

| Platform | Supported storage |
| --- | --- |
| macOS | APFS |
| Linux | Btrfs, reflink-enabled XFS, or a verified OverlayFS setup |
| Windows | ReFS with block cloning |

Ordinary NTFS is unsupported. ext4 needs a working OverlayFS setup rather
than native reflinks.

## Check your destination

```sh
riftri doctor --destination ../app-auth
```

Run this in your repository. Follow its storage and checkout diagnostics.
Linux and Windows also perform active checks during creation.

Keep the repository, destination, and state on the same supported volume for
the simplest setup. The platform guides cover backend-specific requirements.

## Repository features matter too

Custom checkout filters, submodules, required checkout hooks, and external
attributes can block optimization. Supported [Git LFS](git-lfs.md) and
[sparse checkout](sparse-checkout.md) each have specific limits.

Files whose names collide on the destination filesystem cannot be checked out
together, including some case or Unicode variants.

If Riftri cannot reproduce the checkout safely, use ordinary Git outside
Riftri interception. Do not remove required repository behavior to bypass a check.

"""Stream an archive from the descriptor-selected working directory.

Never follow links or cross filesystems. Open each child relative to its parent
descriptor so concurrent renames cannot redirect traversal outside that tree.
"""
import os
import stat
import sys
import tarfile


def archive(directory, name, output, device):
    metadata = os.fstat(directory)
    entry = tarfile.TarInfo(name)
    entry.type = tarfile.DIRTYPE
    entry.mode = 0o755
    entry.mtime = metadata.st_mtime
    output.addfile(entry)
    for child in sorted(os.listdir(directory)):
        metadata = os.stat(child, dir_fd=directory, follow_symlinks=False)
        if metadata.st_dev != device:
            continue
        if not (stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode)):
            continue
        flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
        if stat.S_ISDIR(metadata.st_mode):
            flags |= os.O_DIRECTORY
        fd = os.open(child, flags, dir_fd=directory)
        try:
            actual = os.fstat(fd)
            if actual.st_dev != device:
                continue
            path = name + "/" + child
            if stat.S_ISDIR(actual.st_mode):
                archive(fd, path, output, device)
            elif stat.S_ISREG(actual.st_mode):
                entry = tarfile.TarInfo(path)
                entry.size = actual.st_size
                entry.mode = 0o644
                entry.mtime = actual.st_mtime
                with os.fdopen(os.dup(fd), "rb") as source:
                    output.addfile(entry, source)
        finally:
            os.close(fd)


if __name__ == "__main__":
    directory = os.open(".", os.O_RDONLY | os.O_DIRECTORY)
    try:
        with tarfile.open(fileobj=sys.stdout.buffer, mode="w|gz", format=tarfile.PAX_FORMAT) as output:
            archive(directory, sys.argv[1], output, os.fstat(directory).st_dev)
    finally:
        os.close(directory)

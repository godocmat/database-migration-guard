"""Fetch the pinned Microsoft Windows Server 2025 Evaluation download."""
from concurrent.futures import as_completed, ThreadPoolExecutor
from hashlib import file_digest
from pathlib import Path
import json
import sys
import time
from urllib.request import Request, urlopen

SOURCE = (
    "https://software-static.download.prss.microsoft.com/dbazure/"
    "998969d5-f34g-4e03-ac9d-1f9786c66749/"
    "26100.32230.260111-0550.lt_release_svc_refresh_SERVER_EVAL_x64FRE_en-us.iso"
)
ETAG = '"0x60A8C190FBB54AF58E40BA049FF290D098101E0EAD343CE912A1DC685219BE85"'
SIZE = 8_152_356_864
SHA256 = "7b052573ba7894c9924e3e87ba732ccd354d18cb75a883efa9b900ea125bfd51"
CHUNK_SIZE = 48 * 1024 * 1024


def copy_range(destination, first, last):
    request = Request(SOURCE, headers={
        "Range": f"bytes={first}-{last}",
        "If-Match": ETAG,
        "Accept-Encoding": "identity",
    })
    for attempt in range(3):
        try:
            with urlopen(request, timeout=40) as response, destination.open("r+b") as output:
                if (
                    response.geturl() != SOURCE
                    or response.status != 206
                    or response.headers.get("Content-Range") != f"bytes {first}-{last}/{SIZE}"
                    or response.headers.get("ETag") != ETAG
                ):
                    raise ValueError("Microsoft CDN response changed")
                output.seek(first)
                remaining = last - first + 1
                while remaining:
                    data = response.read(min(1024 * 1024, remaining))
                    if not data:
                        raise OSError("ISO range ended before its declared length")
                    output.write(data)
                    remaining -= len(data)
                if response.read(1):
                    raise ValueError("ISO range exceeded its declared length")
                return
        except OSError:
            if attempt == 2:
                raise
            time.sleep(attempt + 1)


def download(destination):
    with destination.open("xb") as output:
        output.truncate(SIZE)
    with ThreadPoolExecutor(max_workers=10) as pool:
        downloads = [
            pool.submit(copy_range, destination, first, min(first + CHUNK_SIZE, SIZE) - 1)
            for first in range(0, SIZE, CHUNK_SIZE)
        ]
        for count, complete in enumerate(as_completed(downloads), 1):
            complete.result()
            if count % 16 == 0 or count == len(downloads):
                print(f"Received {count} of {len(downloads)} ISO ranges", flush=True)
    with destination.open("rb") as downloaded:
        digest = file_digest(downloaded, "sha256").hexdigest()
    if digest != SHA256:
        raise ValueError(f"Microsoft ISO digest differs: {digest}")
    print(json.dumps({"bytes": SIZE, "etag": ETAG, "sha256": digest, "source": SOURCE}), flush=True)


if __name__ == "__main__":
    download(Path(sys.argv[1]) / "evaluation.iso")

"""build_icon.py
SVG 원본 두 개를 ImageMagick 으로 렌더링해 PNG 압축 다중 해상도 rust-agents-proxy.ico 를 만든다.

실행: uv run --no-project --python "C:/JUNGHO/1.Language/3.Python/python.exe" python assets/icon/build_icon.py
"""

import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

ICON_DIR = Path(__file__).resolve().parent
LARGE_SVG = ICON_DIR / "rust-agents-proxy.svg"
SMALL_SVG = ICON_DIR / "rust-agents-proxy-small.svg"
OUTPUT_ICO = ICON_DIR / "rust-agents-proxy.ico"
# 32px 이하는 링·발광을 뺀 소형 원본에서 축소해야 형태가 남음
SMALL_SIZES = (16, 20, 24, 32)
LARGE_SIZES = (40, 48, 64, 128, 256)


def build_icon(magick: str) -> Path:
    """두 원본을 크기별 8-bit PNG 로 렌더링하고 ICONDIR + PNG 엔트리로 묶는다."""
    with tempfile.TemporaryDirectory(prefix="rap-icon-") as work_text:
        work_dir = Path(work_text)

        # 1. 원본 256px 렌더링 후 크기별 Lanczos 축소
        png_by_size: dict[int, bytes] = {}
        for svg_path, sizes in ((SMALL_SVG, SMALL_SIZES), (LARGE_SVG, LARGE_SIZES)):
            master_png = work_dir / f"{svg_path.stem}-256.png"
            subprocess.run(
                [magick, "-background", "none", "-density", "192", str(svg_path),
                 "-resize", "256x256", "-depth", "8", str(master_png)],
                check=True,
            )
            for size in sizes:
                sized_png = work_dir / f"{svg_path.stem}-{size}.png"
                subprocess.run(
                    [magick, str(master_png), "-filter", "Lanczos", "-resize", f"{size}x{size}",
                     "-depth", "8", f"PNG32:{sized_png}"],
                    check=True,
                )
                png_by_size[size] = sized_png.read_bytes()

        # 2. ICONDIR(6B) + ICONDIRENTRY(16B) * N + PNG 본문 (Vista 이상 PNG 엔트리 지원)
        sizes = sorted(png_by_size)
        header = struct.pack("<HHH", 0, 1, len(sizes))
        entries = b""
        bodies = b""
        offset = 6 + 16 * len(sizes)
        for size in sizes:
            body = png_by_size[size]
            dim = 0 if size >= 256 else size
            entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(body), offset)
            bodies += body
            offset += len(body)
        OUTPUT_ICO.write_bytes(header + entries + bodies)
    return OUTPUT_ICO


if __name__ == "__main__":
    magick_path = shutil.which("magick")
    if magick_path is None:
        sys.exit("ImageMagick(magick) 을 PATH 에서 찾지 못함")
    ico_path = build_icon(magick_path)
    print(f"{ico_path} ({ico_path.stat().st_size} bytes)")

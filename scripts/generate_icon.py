from __future__ import annotations

from io import BytesIO
from pathlib import Path

from PIL import Image
from PySide6.QtCore import QByteArray, QBuffer, QIODevice, QRectF, Qt
from PySide6.QtGui import QColor, QImage, QPainter
from PySide6.QtSvg import QSvgRenderer


PROJECT_ROOT = Path(__file__).resolve().parents[1]
SOURCE = PROJECT_ROOT / "src" / "network_manager" / "web" / "icons" / "network.svg"
OUTPUT = SOURCE.with_name("network-manager.ico")


def render_icon(size: int = 256) -> Image.Image:
    svg = SOURCE.read_text(encoding="utf-8").replace("currentColor", "#ffffff")
    image = QImage(size, size, QImage.Format.Format_ARGB32)
    image.fill(Qt.GlobalColor.transparent)
    painter = QPainter(image)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    painter.setPen(Qt.PenStyle.NoPen)
    painter.setBrush(QColor("#0d8060"))
    painter.drawRoundedRect(QRectF(0, 0, size, size), size * 0.19, size * 0.19)
    renderer = QSvgRenderer(QByteArray(svg.encode("utf-8")))
    margin = size * 0.20
    renderer.render(painter, QRectF(margin, margin, size - margin * 2, size - margin * 2))
    painter.end()

    buffer = QBuffer()
    buffer.open(QIODevice.OpenModeFlag.WriteOnly)
    image.save(buffer, "PNG")
    return Image.open(BytesIO(bytes(buffer.data()))).convert("RGBA")


def main() -> None:
    icon = render_icon()
    icon.save(OUTPUT, format="ICO", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (256, 256)])
    print(f"Icon ready: {OUTPUT}")


if __name__ == "__main__":
    main()

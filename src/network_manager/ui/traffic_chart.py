from __future__ import annotations

from collections import deque

from PySide6.QtCore import QRectF, QSize, Qt
from PySide6.QtGui import QColor, QPainter, QPainterPath, QPen
from PySide6.QtWidgets import QWidget

from network_manager.traffic_monitor import format_rate


DOWNLOAD_COLOR = QColor("#1478e8")
UPLOAD_COLOR = QColor("#e87958")


class TrafficChart(QWidget):
    def __init__(self, max_points: int = 60, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.max_points = max_points
        self.download_rates: deque[float] = deque(maxlen=max_points)
        self.upload_rates: deque[float] = deque(maxlen=max_points)
        self.monitoring = False
        self.setMinimumHeight(142)
        self.setAccessibleName("最近 60 秒实时上传和下载速率曲线")

    def sizeHint(self) -> QSize:
        return QSize(720, 154)

    def reset(self, monitoring: bool = False) -> None:
        self.download_rates.clear()
        self.upload_rates.clear()
        self.monitoring = monitoring
        self.update()

    def append_sample(self, download_rate: float, upload_rate: float) -> None:
        self.monitoring = True
        self.download_rates.append(max(0.0, download_rate))
        self.upload_rates.append(max(0.0, upload_rate))
        self.update()

    def paintEvent(self, _event: object) -> None:
        painter = QPainter(self)
        painter.setRenderHint(QPainter.RenderHint.Antialiasing)
        plot = QRectF(58, 8, max(20, self.width() - 70), max(20, self.height() - 34))

        painter.fillRect(plot, QColor("#fbfcfd"))
        all_rates = [*self.download_rates, *self.upload_rates]
        peak = max(all_rates, default=0.0)
        chart_max = max(1024.0, peak * 1.15)

        painter.setPen(QPen(QColor("#e7ebee"), 1))
        for index in range(4):
            fraction = index / 3
            y = plot.bottom() - plot.height() * fraction
            painter.drawLine(plot.left(), y, plot.right(), y)
            label = format_rate(chart_max * fraction)
            painter.setPen(QColor("#8a949e"))
            painter.drawText(
                QRectF(0, y - 9, 50, 18),
                Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter,
                label,
            )
            painter.setPen(QPen(QColor("#e7ebee"), 1))

        painter.setPen(QColor("#8a949e"))
        painter.drawText(
            QRectF(plot.left(), plot.bottom() + 5, 70, 18),
            Qt.AlignmentFlag.AlignLeft,
            "60 秒前",
        )
        painter.drawText(
            QRectF(plot.right() - 50, plot.bottom() + 5, 50, 18),
            Qt.AlignmentFlag.AlignRight,
            "现在",
        )

        if not self.download_rates:
            message = "正在连接流量统计..." if self.monitoring else "接管停止"
            painter.setPen(QColor("#8a949e"))
            painter.drawText(plot, Qt.AlignmentFlag.AlignCenter, message)
            return

        self._draw_series(painter, plot, list(self.upload_rates), chart_max, UPLOAD_COLOR)
        self._draw_series(
            painter, plot, list(self.download_rates), chart_max, DOWNLOAD_COLOR
        )

    def _draw_series(
        self,
        painter: QPainter,
        plot: QRectF,
        values: list[float],
        chart_max: float,
        color: QColor,
    ) -> None:
        path = QPainterPath()
        step = plot.width() / max(1, self.max_points - 1)
        start_index = self.max_points - len(values)
        for index, value in enumerate(values):
            x = plot.left() + (start_index + index) * step
            y = plot.bottom() - min(1.0, value / chart_max) * plot.height()
            if index == 0:
                path.moveTo(x, y)
            else:
                path.lineTo(x, y)
        painter.setBrush(Qt.BrushStyle.NoBrush)
        painter.setPen(QPen(color, 2))
        painter.drawPath(path)
        end = path.currentPosition()
        painter.setBrush(color)
        painter.setPen(Qt.PenStyle.NoPen)
        painter.drawEllipse(end, 3, 3)

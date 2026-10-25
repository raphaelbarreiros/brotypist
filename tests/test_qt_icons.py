"""Small and large logical icon requests must retain different visible artwork."""
import pytest

pytest.importorskip("PySide6")
from PySide6.QtCore import QSize
from PySide6.QtWidgets import QApplication, QWidget

from inklow.resources import configure_identity


@pytest.mark.parametrize("logical_size,dpr,pills", [
    (16, 1.0, 1), (16, 2.0, 1),
    (24, 1.0, 1), (24, 2.0, 1),
    (32, 1.0, 1), (32, 2.0, 1),
    (33, 1.0, 2), (33, 2.0, 2),
    (64, 1.0, 2), (64, 2.0, 2),
])
def test_window_icon_uses_logical_size_for_visible_ghost_pills(logical_size, dpr, pills):
    app = QApplication.instance() or QApplication([])
    configure_identity(app)
    window = QWidget()
    pixmap = window.windowIcon().pixmap(QSize(logical_size, logical_size), dpr)
    assert pixmap.devicePixelRatio() == dpr
    assert pixmap.width() == round(logical_size * dpr)
    image = pixmap.toImage()
    assert not image.isNull()
    # Count separated mint strokes across the center row, not hashes or a copy
    # of the source asset. At equal physical size, 32pt@2x needs one pill while
    # 64pt@1x needs two; routing by raster dimensions cannot satisfy both.
    runs, in_pill = 0, False
    for x in range(image.width()):
        color = image.pixelColor(x, image.height() // 2)
        mint = (color.alpha() > 128 and color.green() - color.red() > 20
                and color.green() - color.blue() > 5)
        if mint and not in_pill:
            runs += 1
        in_pill = mint
    assert runs == pills
    window.close()

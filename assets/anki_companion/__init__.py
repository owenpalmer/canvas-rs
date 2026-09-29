"""canvas-mcp companion add-on for Anki.

When the Canvas app starts Anki in the tray (it sets CANVAS_ANKI_TRAY=1), Anki opens hidden with a
tray icon instead of a window. Started any other way, Anki behaves exactly as usual.

Installed and updated by canvas-mcp (crates/canvas-mcp/src/anki_setup.rs); edits here are overwritten.
"""

import os

from aqt import gui_hooks, mw
from aqt.qt import QMenu, QSystemTrayIcon, QTimer

TRAY = os.environ.get("CANVAS_ANKI_TRAY") == "1"
_tray = None
_menu = None


def show_window():
    mw.show()
    mw.showNormal()
    mw.raise_()
    mw.activateWindow()


def _on_tray(reason):
    if reason in (QSystemTrayIcon.ActivationReason.Trigger, QSystemTrayIcon.ActivationReason.DoubleClick):
        if mw.isVisible():
            mw.hide()
        else:
            show_window()


def _setup_tray():
    global _tray, _menu
    if _tray is not None:
        return
    _menu = QMenu()
    _menu.addAction("Show Anki").triggered.connect(show_window)
    _menu.addAction("Quit Anki").triggered.connect(mw.close)  # Anki's own close: saves, may sync
    _tray = QSystemTrayIcon(mw.windowIcon(), mw)
    _tray.setToolTip("Anki")
    _tray.setContextMenu(_menu)
    _tray.activated.connect(_on_tray)
    _tray.show()


def _on_app_msg(buf):
    # Launching Anki again while it runs sends "raise"; Anki raises the window, but a hidden one
    # also has to be shown. The Canvas app's "Show Anki" button works this way.
    if buf == "raise":
        show_window()


def _on_profile_open():
    _setup_tray()
    mw.app.appMsg.connect(_on_app_msg)
    QTimer.singleShot(0, mw.hide)


if TRAY:
    gui_hooks.profile_did_open.append(_on_profile_open)

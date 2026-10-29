"""Multi Point Clicker

最大10か所のクリック位置を記憶し、指定した間隔で順番に自動クリックするツール (Windows向け)。

ホットキー:
    F6 : 記憶モード開始 (以降の左クリック位置を記録)
    F7 : 記憶モード終了 (記録した位置を確定・保存)
    F8 : 自動クリック開始
    F9 : 自動クリック停止
"""

import ctypes
import json
import os
import queue
import sys
import threading
import tkinter as tk
from tkinter import ttk

# 高DPI環境で座標がずれないようにする (tkinter / pynput より前に設定する)
if sys.platform == "win32":
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
    except Exception:
        try:
            ctypes.windll.user32.SetProcessDPIAware()
        except Exception:
            pass

from pynput import keyboard, mouse  # noqa: E402

MAX_POINTS = 10
DEFAULT_INTERVAL = 1.0
MIN_INTERVAL = 0.05

HOTKEY_RECORD_START = keyboard.Key.f6
HOTKEY_RECORD_STOP = keyboard.Key.f7
HOTKEY_CLICK_START = keyboard.Key.f8
HOTKEY_CLICK_STOP = keyboard.Key.f9

IDLE, RECORDING, CLICKING = "idle", "recording", "clicking"
STATE_LABELS = {
    IDLE: ("待機中", "#555555"),
    RECORDING: ("記憶モード (左クリックで位置を記録)", "#c0392b"),
    CLICKING: ("自動クリック中", "#27ae60"),
}


def app_dir():
    if getattr(sys, "frozen", False):
        return os.path.dirname(sys.executable)
    return os.path.dirname(os.path.abspath(__file__))


SAVE_FILE = os.path.join(app_dir(), "clicker_settings.json")


class ClickerApp:
    def __init__(self, root):
        self.root = root
        self.state = IDLE
        self.points = []
        self.recording_points = []
        self.stop_event = threading.Event()
        self.click_thread = None
        self.events = queue.Queue()
        self.window_rect = (0, 0, 0, 0)
        self.mouse_ctl = mouse.Controller()

        self.interval_var = tk.StringVar(value=str(DEFAULT_INTERVAL))
        self.topmost_var = tk.BooleanVar(value=True)
        self.load()

        self.build_ui()
        self.refresh()

        self.kb_listener = keyboard.Listener(on_press=self.on_key_press)
        self.mouse_listener = mouse.Listener(on_click=self.on_mouse_click)
        self.kb_listener.start()
        self.mouse_listener.start()

        self.root.protocol("WM_DELETE_WINDOW", self.on_close)
        self.root.after(50, self.poll)

    # ---------- UI ----------
    def build_ui(self):
        self.root.title("Multi Point Clicker")
        self.root.resizable(False, False)
        frm = ttk.Frame(self.root, padding=12)
        frm.grid()

        self.status_label = tk.Label(frm, font=("Meiryo UI", 12, "bold"), anchor="w")
        self.status_label.grid(row=0, column=0, columnspan=3, sticky="we", pady=(0, 8))

        ttk.Label(frm, text="クリック間隔 (秒):").grid(row=1, column=0, sticky="w")
        self.interval_entry = ttk.Entry(frm, textvariable=self.interval_var, width=8)
        self.interval_entry.grid(row=1, column=1, sticky="w")
        ttk.Checkbutton(
            frm, text="常に手前に表示", variable=self.topmost_var, command=self.apply_topmost
        ).grid(row=1, column=2, sticky="e")

        ttk.Label(frm, text=f"記憶しているクリック位置 (最大{MAX_POINTS}か所):").grid(
            row=2, column=0, columnspan=3, sticky="w", pady=(10, 2)
        )
        self.listbox = tk.Listbox(frm, height=MAX_POINTS, width=40, font=("Consolas", 10))
        self.listbox.grid(row=3, column=0, columnspan=3, sticky="we")

        help_text = (
            "F6: 記憶モード開始    F7: 記憶モード終了\n"
            "F8: 自動クリック開始  F9: 自動クリック停止"
        )
        ttk.Label(frm, text=help_text, foreground="#333").grid(
            row=4, column=0, columnspan=3, sticky="w", pady=(10, 0)
        )
        self.apply_topmost()

    def apply_topmost(self):
        self.root.attributes("-topmost", self.topmost_var.get())

    def refresh(self):
        text, color = STATE_LABELS[self.state]
        if self.state == RECORDING:
            text += f"  {len(self.recording_points)}/{MAX_POINTS}"
        self.status_label.config(text=text, fg=color)

        shown = self.recording_points if self.state == RECORDING else self.points
        self.listbox.delete(0, tk.END)
        for i, (x, y) in enumerate(shown, 1):
            self.listbox.insert(tk.END, f"{i:2d}:  X={x:5d}  Y={y:5d}")
        if not shown:
            self.listbox.insert(tk.END, "  (未設定)")

        self.interval_entry.config(state="disabled" if self.state == CLICKING else "normal")

    # ---------- listener callbacks (別スレッド) ----------
    def on_key_press(self, key):
        if key in (HOTKEY_RECORD_START, HOTKEY_RECORD_STOP, HOTKEY_CLICK_START, HOTKEY_CLICK_STOP):
            self.events.put(("key", key))

    def on_mouse_click(self, x, y, button, pressed):
        if pressed and button == mouse.Button.left and self.state == RECORDING:
            self.events.put(("click", (int(x), int(y))))

    # ---------- main thread ----------
    def poll(self):
        try:
            self.root.update_idletasks()
            self.window_rect = (
                self.root.winfo_rootx(),
                self.root.winfo_rooty(),
                self.root.winfo_rootx() + self.root.winfo_width(),
                self.root.winfo_rooty() + self.root.winfo_height(),
            )
        except tk.TclError:
            return
        while True:
            try:
                kind, value = self.events.get_nowait()
            except queue.Empty:
                break
            if kind == "key":
                self.handle_key(value)
            elif kind == "click":
                self.handle_record_click(value)
        if self.state == CLICKING and self.click_thread and not self.click_thread.is_alive():
            self.state = IDLE
            self.refresh()
        self.root.after(30, self.poll)

    def handle_key(self, key):
        if key == HOTKEY_RECORD_START and self.state == IDLE:
            self.recording_points = []
            self.state = RECORDING
        elif key == HOTKEY_RECORD_STOP and self.state == RECORDING:
            # 1か所も記録しなかった場合は以前の位置をそのまま残す
            if self.recording_points:
                self.points = self.recording_points
                self.save()
            self.recording_points = []
            self.state = IDLE
        elif key == HOTKEY_CLICK_START and self.state == IDLE:
            self.start_clicking()
        elif key == HOTKEY_CLICK_STOP and self.state == CLICKING:
            self.stop_clicking()
        self.refresh()

    def handle_record_click(self, pos):
        if self.state != RECORDING:
            return
        x, y = pos
        left, top, right, bottom = self.window_rect
        if left <= x < right and top <= y < bottom:
            return  # このツール自身のウィンドウ上のクリックは記録しない
        if len(self.recording_points) < MAX_POINTS:
            self.recording_points.append(pos)
            self.refresh()

    def get_interval(self):
        try:
            value = float(self.interval_var.get())
        except ValueError:
            value = DEFAULT_INTERVAL
        value = max(MIN_INTERVAL, value)
        self.interval_var.set(str(value))
        return value

    def start_clicking(self):
        if not self.points:
            self.status_label.config(text="クリック位置が未設定です (F6で記憶)", fg="#c0392b")
            return
        interval = self.get_interval()
        self.save()
        self.stop_event.clear()
        self.state = CLICKING
        self.click_thread = threading.Thread(
            target=self.click_loop, args=(list(self.points), interval), daemon=True
        )
        self.click_thread.start()

    def stop_clicking(self):
        self.stop_event.set()
        self.state = IDLE

    def click_loop(self, points, interval):
        while not self.stop_event.is_set():
            for x, y in points:
                if self.stop_event.is_set():
                    return
                self.mouse_ctl.position = (x, y)
                self.mouse_ctl.click(mouse.Button.left)
                if self.stop_event.wait(interval):
                    return

    # ---------- 保存 ----------
    def load(self):
        try:
            with open(SAVE_FILE, encoding="utf-8") as f:
                data = json.load(f)
            self.points = [(int(x), int(y)) for x, y in data.get("points", [])][:MAX_POINTS]
            self.interval_var.set(str(data.get("interval", DEFAULT_INTERVAL)))
            self.topmost_var.set(bool(data.get("topmost", True)))
        except (OSError, ValueError, TypeError):
            pass

    def save(self):
        data = {
            "points": self.points,
            "interval": self.get_interval(),
            "topmost": self.topmost_var.get(),
        }
        try:
            with open(SAVE_FILE, "w", encoding="utf-8") as f:
                json.dump(data, f, ensure_ascii=False, indent=2)
        except OSError:
            pass

    def on_close(self):
        self.stop_event.set()
        self.save()
        self.kb_listener.stop()
        self.mouse_listener.stop()
        self.root.destroy()


def main():
    root = tk.Tk()
    ClickerApp(root)
    root.mainloop()


if __name__ == "__main__":
    main()

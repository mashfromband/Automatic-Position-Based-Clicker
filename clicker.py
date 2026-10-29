"""Multi Point Clicker

Remember up to 10 click positions and click them in order at a fixed interval.
最大10か所のクリック位置を記憶し、指定間隔で順番に自動クリックするツール (Windows向け)。
"""

import ctypes
import json
import locale
import os
import queue
import re
import sys
import threading
import time
import tkinter as tk

# 高DPI環境でも座標がずれないよう、GUIライブラリより先にDPI対応を宣言する
if sys.platform == "win32":
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
    except Exception:
        try:
            ctypes.windll.user32.SetProcessDPIAware()
        except Exception:
            pass

import customtkinter as ctk  # noqa: E402
from pynput import keyboard, mouse  # noqa: E402

from i18n import LANGS, STRINGS  # noqa: E402

MAX_POINTS = 10
MIN_INTERVAL = 0.05
MAX_INTERVAL = 3600.0
MAX_DELAY = 30
INTERVAL_PRESETS = (0.1, 0.5, 1, 2, 5)

IDLE, RECORDING, COUNTDOWN, CLICKING = "idle", "recording", "countdown", "clicking"

# ファンクションキー/テンキーが無い環境でも使えるよう、Ctrl+Alt+英字を初期値にする
DEFAULT_HOTKEYS = {
    "record_start": "ctrl+alt+r",
    "record_stop": "ctrl+alt+d",
    "click_start": "ctrl+alt+s",
    "click_stop": "ctrl+alt+x",
}
HOTKEY_ACTIONS = tuple(DEFAULT_HOTKEYS)

IS_WIN = sys.platform == "win32"

# ---------- colors ----------
ACCENT = ("#2563eb", "#3b82f6")
ACCENT_HOVER = ("#1d4ed8", "#2563eb")
RED = ("#dc2626", "#ef4444")
RED_HOVER = ("#b91c1c", "#dc2626")
GREEN = ("#16a34a", "#22c55e")
GREEN_HOVER = ("#15803d", "#16a34a")
AMBER = ("#d97706", "#f59e0b")
GRAY = ("#6b7280", "#6b7280")
CARD = ("#ffffff", "#1f2430")
CARD_ALT = ("#f1f5f9", "#2a3040")
TEXT_MUTED = ("#64748b", "#94a3b8")
DISABLED_BG = ("#e2e8f0", "#2f3646")
DISABLED_FG = ("#94a3b8", "#6b7280")
STATUS_COLORS = {IDLE: GRAY, RECORDING: RED, COUNTDOWN: AMBER, CLICKING: GREEN}


# ---------- hotkey helpers ----------
MOD_ORDER = ("ctrl", "alt", "shift", "win")
MOD_NAMES = {
    "ctrl": "ctrl", "ctrl_l": "ctrl", "ctrl_r": "ctrl",
    "alt": "alt", "alt_l": "alt", "alt_r": "alt",
    "alt_gr": "altgr",
    "shift": "shift", "shift_l": "shift", "shift_r": "shift",
    "cmd": "win", "cmd_l": "win", "cmd_r": "win",
}


def modifier_of(key):
    if isinstance(key, keyboard.Key):
        return MOD_NAMES.get(key.name)
    return None


def main_token(key):
    """修飾キー以外のキーを、レイアウトに依存しにくい文字列にする。"""
    if isinstance(key, keyboard.Key):
        return key.name
    vk = getattr(key, "vk", None)
    if IS_WIN and vk:
        if 0x41 <= vk <= 0x5A or 0x30 <= vk <= 0x39:
            return chr(vk).lower()
        if 0x60 <= vk <= 0x69:
            return f"num{vk - 0x60}"
    ch = getattr(key, "char", None)
    if ch and ch.isprintable() and not ch.isspace():
        return ch.lower()
    if vk is not None:
        return f"vk{vk}"
    return None


def make_combo(mods, token):
    mods = set(mods)
    if "altgr" in mods:
        mods |= {"ctrl", "alt"}
        mods.discard("altgr")
    return "+".join([m for m in MOD_ORDER if m in mods] + [token])


def format_combo(combo):
    parts = []
    for p in combo.split("+"):
        if p in MOD_ORDER:
            parts.append({"ctrl": "Ctrl", "alt": "Alt", "shift": "Shift", "win": "Win"}[p])
        elif len(p) == 1 or re.fullmatch(r"f\d+", p):
            parts.append(p.upper())
        else:
            parts.append(p.replace("_", " ").title())
    return " + ".join(parts)


# ---------- paths / settings ----------
def settings_path():
    if IS_WIN:
        base = os.environ.get("APPDATA") or os.path.expanduser("~")
    else:
        base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    folder = os.path.join(base, "MultiPointClicker")
    try:
        os.makedirs(folder, exist_ok=True)
    except OSError:
        pass
    return os.path.join(folder, "settings.json")


def detect_language():
    try:
        if IS_WIN:
            if ctypes.windll.kernel32.GetUserDefaultUILanguage() & 0x3FF == 0x11:
                return "ja"
        else:
            if (locale.getlocale()[0] or "").lower().startswith("ja"):
                return "ja"
    except Exception:
        pass
    return "en"


def format_elapsed(seconds):
    seconds = int(seconds)
    h, rem = divmod(seconds, 3600)
    m, s = divmod(rem, 60)
    return f"{h:d}:{m:02d}:{s:02d}" if h else f"{m:02d}:{s:02d}"


def clamp(value, lo, hi):
    return max(lo, min(hi, value))


# ---------- on-screen marker ----------
class Marker:
    """クリック位置に番号付きの丸を表示する (Windowsではクリックを素通しする)。"""

    SIZE = 34
    KEY = "#010203"

    def __init__(self, root, number, x, y):
        s = self.SIZE
        self.win = tk.Toplevel(root)
        self.win.overrideredirect(True)
        self.win.attributes("-topmost", True)
        self.win.geometry(f"{s}x{s}+{x - s // 2}+{y - s // 2}")
        canvas = tk.Canvas(self.win, width=s, height=s, highlightthickness=0, bg=self.KEY)
        canvas.pack()
        canvas.create_oval(2, 2, s - 2, s - 2, fill="#2563eb", outline="#ffffff", width=2)
        canvas.create_text(s // 2, s // 2, text=str(number), fill="#ffffff", font=("Arial", 12, "bold"))
        try:
            self.win.attributes("-transparentcolor", self.KEY)
        except tk.TclError:
            pass
        self.win.update_idletasks()
        if IS_WIN:
            try:
                user32 = ctypes.windll.user32
                hwnd = user32.GetParent(self.win.winfo_id()) or self.win.winfo_id()
                style = user32.GetWindowLongW(hwnd, -20)
                # LAYERED | TRANSPARENT (click-through) | TOOLWINDOW | NOACTIVATE
                user32.SetWindowLongW(hwnd, -20, style | 0x80000 | 0x20 | 0x80 | 0x08000000)
            except Exception:
                pass

    def destroy(self):
        try:
            self.win.destroy()
        except tk.TclError:
            pass


# ---------- app ----------
class ClickerApp:
    def __init__(self, root):
        self.root = root
        self.save_file = settings_path()

        # persistent settings
        self.lang = detect_language()
        self.theme = "system"
        self.points = []
        self.interval = 1.0
        self.repeat = 0
        self.delay = 0
        self.button = "left"
        self.topmost = False
        self.hotkeys = dict(DEFAULT_HOTKEYS)
        self.load()

        # runtime state
        self.state = IDLE
        self.recording_points = []
        self.message = ""
        self.message_until = 0.0
        self.capturing = None          # hotkey action being captured
        self.pressed_mods = set()
        self.held_keys = set()
        self.events = queue.Queue()
        self.stop_event = threading.Event()
        self.click_thread = None
        self.mouse_ctl = mouse.Controller()
        self.stats = {"clicks": 0, "cycles": 0, "start": 0.0, "countdown": 0.0}
        self.markers = []
        self.marker_job = None
        self.tab_index = 0

        ctk.set_appearance_mode(self.theme)
        ctk.set_default_color_theme("blue")
        self.root.minsize(500, 780)
        self.root.geometry("520x820")
        self.root.attributes("-topmost", self.topmost)

        self.kb_listener = keyboard.Listener(on_press=self.on_key_press, on_release=self.on_key_release)
        self.mouse_listener = mouse.Listener(on_click=self.on_mouse_click)
        self.kb_listener.start()
        self.mouse_listener.start()

        self.container = None
        self.build_ui()
        self.root.protocol("WM_DELETE_WINDOW", self.on_close)
        self.root.after(50, self.poll)

    # ---------- i18n / fonts ----------
    def t(self, key, **kw):
        text = STRINGS[self.lang][key]
        return text.format(**kw) if kw else text

    def font(self, size=13, weight="normal"):
        family = "Yu Gothic UI" if (IS_WIN and self.lang == "ja") else ("Segoe UI" if IS_WIN else None)
        return ctk.CTkFont(family=family, size=size, weight=weight)

    def hk_text(self, action):
        return format_combo(self.hotkeys[action])

    # ---------- UI ----------
    def build_ui(self):
        if self.container is not None:
            self.container.destroy()
        self.root.title(self.t("app_title"))
        self.container = ctk.CTkFrame(self.root, fg_color="transparent")
        self.container.pack(fill="both", expand=True, padx=14, pady=(12, 14))

        header = ctk.CTkFrame(self.container, fg_color="transparent")
        header.pack(fill="x", pady=(0, 8))
        ctk.CTkLabel(header, text="◎  " + self.t("app_title"), font=self.font(18, "bold")).pack(side="left")
        self.lang_seg = ctk.CTkSegmentedButton(
            header, values=list(LANGS.values()), command=self.on_lang_change, font=self.font(12), width=150
        )
        self.lang_seg.set(LANGS[self.lang])
        self.lang_seg.pack(side="right")

        names = [self.t("tab_main"), self.t("tab_settings")]
        self.tabs = ctk.CTkTabview(self.container, fg_color="transparent", command=self.on_tab_change)
        self.tabs._segmented_button.configure(font=self.font(13))
        for n in names:
            self.tabs.add(n)
        self.tabs.pack(fill="both", expand=True)
        self.build_main(self.tabs.tab(names[0]))
        self.build_settings(self.tabs.tab(names[1]))
        self.tabs.set(names[self.tab_index])
        self.refresh()

    def on_tab_change(self):
        names = [self.t("tab_main"), self.t("tab_settings")]
        try:
            self.tab_index = names.index(self.tabs.get())
        except ValueError:
            pass

    def card(self, parent, **kw):
        return ctk.CTkFrame(parent, corner_radius=14, fg_color=CARD, **kw)

    def build_main(self, tab):
        # --- status card ---
        status = self.card(tab)
        status.pack(fill="x", pady=(2, 8))
        top = ctk.CTkFrame(status, fg_color="transparent")
        top.pack(fill="x", padx=14, pady=(12, 0))
        self.status_pill = ctk.CTkLabel(
            top, text="", corner_radius=14, height=28, width=150, text_color="#ffffff", font=self.font(13, "bold")
        )
        self.status_pill.pack(side="left")
        self.hint_label = ctk.CTkLabel(
            status, text="", font=self.font(12), text_color=TEXT_MUTED, wraplength=430, justify="left", anchor="w"
        )
        self.hint_label.pack(fill="x", padx=14, pady=(8, 0))
        self.message_label = ctk.CTkLabel(
            status, text="", font=self.font(12, "bold"), text_color=ACCENT, wraplength=430, justify="left", anchor="w"
        )
        self.message_label.pack(fill="x", padx=14, pady=(2, 0))

        stats = ctk.CTkFrame(status, fg_color="transparent")
        stats.pack(fill="x", padx=10, pady=(6, 12))
        self.stat_labels = {}
        for i, key in enumerate(("clicks", "cycles", "time")):
            stats.grid_columnconfigure(i, weight=1, uniform="stat")
            cell = ctk.CTkFrame(stats, corner_radius=10, fg_color=CARD_ALT)
            cell.grid(row=0, column=i, padx=4, sticky="nsew")
            value = ctk.CTkLabel(cell, text="0", font=self.font(20, "bold"))
            value.pack(pady=(6, 0))
            ctk.CTkLabel(cell, text=self.t("stat_" + key), font=self.font(11), text_color=TEXT_MUTED).pack(pady=(0, 6))
            self.stat_labels[key] = value

        # --- action buttons ---
        actions = self.card(tab)
        actions.pack(fill="x", pady=(0, 8))
        actions.grid_columnconfigure((0, 1), weight=1, uniform="act")
        for col, step in enumerate(("step_record", "step_click")):
            ctk.CTkLabel(actions, text=self.t(step), font=self.font(12, "bold"), text_color=TEXT_MUTED, anchor="w").grid(
                row=0, column=col, sticky="w", padx=14, pady=(10, 2)
            )
        self.btns = {}
        self.btn_colors = {}
        specs = (
            ("record_start", 1, 0, RED, RED_HOVER, self.start_recording),
            ("record_stop", 2, 0, ACCENT, ACCENT_HOVER, self.stop_recording),
            ("click_start", 1, 1, GREEN, GREEN_HOVER, self.start_clicking),
            ("click_stop", 2, 1, GRAY, ("#4b5563", "#4b5563"), self.stop_clicking),
        )
        for action, row, col, color, hover, cmd in specs:
            btn = ctk.CTkButton(
                actions, text="", height=52, corner_radius=10, fg_color=color, hover_color=hover,
                font=self.font(13, "bold"), command=cmd,
            )
            btn.grid(row=row, column=col, sticky="ew", padx=(12 if col == 0 else 5, 5 if col == 0 else 12), pady=(2, 8))
            self.btns[action] = btn
            self.btn_colors[action] = (color, hover)
        actions.grid_rowconfigure(2, pad=2)

        # --- quick settings ---
        quick = self.card(tab)
        quick.pack(fill="x", pady=(0, 8))
        row1 = ctk.CTkFrame(quick, fg_color="transparent")
        row1.pack(fill="x", padx=14, pady=(12, 4))
        ctk.CTkLabel(row1, text=self.t("interval"), font=self.font(13, "bold")).pack(side="left")
        ctk.CTkLabel(row1, text=self.t("seconds"), font=self.font(12), text_color=TEXT_MUTED).pack(side="right")
        self.interval_entry = ctk.CTkEntry(row1, width=76, justify="center", font=self.font(13))
        self.interval_entry.insert(0, f"{self.interval:g}")
        self.interval_entry.pack(side="right", padx=6)
        self.interval_entry.bind("<Return>", lambda e: self.commit_numbers())
        self.interval_entry.bind("<FocusOut>", lambda e: self.commit_numbers())

        chips = ctk.CTkFrame(quick, fg_color="transparent")
        chips.pack(fill="x", padx=12, pady=(0, 6))
        self.chips = []
        for v in INTERVAL_PRESETS:
            chip = ctk.CTkButton(
                chips, text=f"{v:g}", width=44, height=26, corner_radius=13, fg_color=CARD_ALT,
                text_color=("#0f172a", "#e2e8f0"), hover_color=("#e2e8f0", "#374151"), font=self.font(12),
                command=lambda v=v: self.set_interval(v),
            )
            chip.pack(side="left", padx=2)
            self.chips.append(chip)

        row2 = ctk.CTkFrame(quick, fg_color="transparent")
        row2.pack(fill="x", padx=14, pady=(2, 12))
        ctk.CTkLabel(row2, text=self.t("repeat"), font=self.font(13, "bold")).pack(side="left")
        ctk.CTkLabel(row2, text=f"{self.t('times')}  ({self.t('repeat_hint')})", font=self.font(12), text_color=TEXT_MUTED).pack(side="right")
        self.repeat_entry = ctk.CTkEntry(row2, width=76, justify="center", font=self.font(13))
        self.repeat_entry.insert(0, str(self.repeat))
        self.repeat_entry.pack(side="right", padx=6)
        self.repeat_entry.bind("<Return>", lambda e: self.commit_numbers())
        self.repeat_entry.bind("<FocusOut>", lambda e: self.commit_numbers())

        # --- points ---
        points = self.card(tab)
        points.pack(fill="both", expand=True)
        head = ctk.CTkFrame(points, fg_color="transparent")
        head.pack(fill="x", padx=14, pady=(10, 4))
        self.points_title = ctk.CTkLabel(head, text="", font=self.font(13, "bold"))
        self.points_title.pack(side="left")
        self.clear_btn = ctk.CTkButton(
            head, text=self.t("clear_all"), width=70, height=26, fg_color="transparent", border_width=1,
            text_color=("#334155", "#cbd5e1"), border_color=("#cbd5e1", "#475569"),
            hover_color=("#f1f5f9", "#2a3040"), font=self.font(12), command=self.clear_points,
        )
        self.clear_btn.pack(side="right")
        self.preview_btn = ctk.CTkButton(
            head, text=self.t("preview"), width=90, height=26, fg_color="transparent", border_width=1,
            text_color=("#334155", "#cbd5e1"), border_color=("#cbd5e1", "#475569"),
            hover_color=("#f1f5f9", "#2a3040"), font=self.font(12), command=self.preview_markers,
        )
        self.preview_btn.pack(side="right", padx=6)
        self.points_frame = ctk.CTkScrollableFrame(points, fg_color="transparent", height=150)
        self.points_frame.pack(fill="both", expand=True, padx=6, pady=(0, 8))

    def build_settings(self, tab):
        scroll = ctk.CTkScrollableFrame(tab, fg_color="transparent")
        scroll.pack(fill="both", expand=True)

        def section(title):
            ctk.CTkLabel(scroll, text=title, font=self.font(12, "bold"), text_color=TEXT_MUTED, anchor="w").pack(
                fill="x", padx=6, pady=(8, 3)
            )
            card = self.card(scroll)
            card.pack(fill="x", pady=(0, 4))
            return card

        def row(card, label):
            r = ctk.CTkFrame(card, fg_color="transparent")
            r.pack(fill="x", padx=14, pady=7)
            ctk.CTkLabel(r, text=label, font=self.font(13), anchor="w").pack(side="left")
            return r

        # behavior
        card = section(self.t("sec_behavior"))
        r = row(card, self.t("click_button"))
        self.button_seg = ctk.CTkSegmentedButton(
            r, values=[self.t("left"), self.t("right")], command=self.on_button_change, font=self.font(12)
        )
        self.button_seg.set(self.t("left") if self.button == "left" else self.t("right"))
        self.button_seg.pack(side="right")
        r = row(card, self.t("start_delay"))
        self.delay_label = ctk.CTkLabel(r, text="", font=self.font(12), width=44, text_color=TEXT_MUTED)
        self.delay_label.pack(side="right")
        self.delay_slider = ctk.CTkSlider(r, from_=0, to=MAX_DELAY, number_of_steps=MAX_DELAY, width=170, command=self.on_delay_change)
        self.delay_slider.set(self.delay)
        self.delay_slider.pack(side="right", padx=8)
        self.on_delay_change(self.delay, save=False)

        # hotkeys
        card = section(self.t("sec_hotkeys"))
        ctk.CTkLabel(
            card, text=self.t("hotkeys_note"), font=self.font(12), text_color=TEXT_MUTED, wraplength=400, justify="left", anchor="w"
        ).pack(fill="x", padx=14, pady=(10, 0))
        self.hk_buttons = {}
        for action in HOTKEY_ACTIONS:
            r = row(card, self.t("hk_" + action))
            ctk.CTkButton(
                r, text="↺", width=30, height=30, fg_color="transparent", border_width=1,
                text_color=("#334155", "#cbd5e1"), border_color=("#cbd5e1", "#475569"),
                hover_color=("#f1f5f9", "#2a3040"), font=self.font(14),
                command=lambda a=action: self.reset_hotkey(a),
            ).pack(side="right")
            b = ctk.CTkButton(
                r, text="", width=170, height=30, fg_color=CARD_ALT, text_color=("#0f172a", "#e2e8f0"),
                hover_color=("#e2e8f0", "#374151"), font=self.font(13, "bold"),
                command=lambda a=action: self.begin_capture(a),
            )
            b.pack(side="right", padx=6)
            self.hk_buttons[action] = b
        ctk.CTkButton(
            card, text=self.t("hk_reset_all"), height=30, fg_color="transparent", border_width=1,
            text_color=("#334155", "#cbd5e1"), border_color=("#cbd5e1", "#475569"),
            hover_color=("#f1f5f9", "#2a3040"), font=self.font(12), command=self.reset_all_hotkeys,
        ).pack(fill="x", padx=14, pady=(4, 12))

        # appearance
        card = section(self.t("sec_appearance"))
        r = row(card, self.t("theme"))
        theme_names = {"system": self.t("theme_system"), "light": self.t("theme_light"), "dark": self.t("theme_dark")}
        self.theme_names = theme_names
        self.theme_menu = ctk.CTkOptionMenu(
            r, values=list(theme_names.values()), command=self.on_theme_change, font=self.font(12),
            dropdown_font=self.font(12), width=130,
        )
        self.theme_menu.set(theme_names[self.theme])
        self.theme_menu.pack(side="right")
        r = row(card, self.t("topmost"))
        self.topmost_switch = ctk.CTkSwitch(r, text="", command=self.on_topmost_change, width=46)
        if self.topmost:
            self.topmost_switch.select()
        self.topmost_switch.pack(side="right")

    # ---------- refresh ----------
    def refresh(self):
        """状態に合わせて表示を更新する。"""
        s = self.state
        self.status_pill.configure(text="● " + self.t("status_" + s), fg_color=STATUS_COLORS[s])

        # buttons
        enabled = {
            "record_start": s == IDLE,
            "record_stop": s == RECORDING,
            "click_start": s == IDLE,
            "click_stop": s in (COUNTDOWN, CLICKING),
        }
        for action, btn in self.btns.items():
            color, hover = self.btn_colors[action]
            on = enabled[action]
            btn.configure(
                text=f"{self.t('btn_' + action)}\n{self.hk_text(action)}",
                state="normal" if on else "disabled",
                fg_color=color if on else DISABLED_BG,
                hover_color=hover if on else DISABLED_BG,
                text_color=("#ffffff", "#ffffff") if on else DISABLED_FG,
                text_color_disabled=DISABLED_FG,
            )
        for action, btn in self.hk_buttons.items():
            if self.capturing == action:
                btn.configure(text=self.t("hk_press"), fg_color=ACCENT, text_color="#ffffff")
            else:
                btn.configure(text=self.hk_text(action), fg_color=CARD_ALT, text_color=("#0f172a", "#e2e8f0"))

        busy = s in (RECORDING, COUNTDOWN, CLICKING)
        for w in (self.interval_entry, self.repeat_entry):
            w.configure(state="disabled" if busy else "normal")
        for chip in self.chips:
            chip.configure(state="disabled" if busy else "normal")
        self.clear_btn.configure(state="disabled" if busy or not self.points else "normal")
        self.preview_btn.configure(state="disabled" if busy or not self.points else "normal")

        self.update_hint()
        self.refresh_points()
        self.refresh_stats()
        self.update_markers_for_state()

    def update_hint(self):
        s = self.state
        if s == IDLE:
            text = self.t("hint_idle")
        elif s == RECORDING:
            text = self.t("hint_recording", max=MAX_POINTS)
        elif s == COUNTDOWN:
            text = self.t("hint_countdown", sec=max(1, int(self.stats["countdown"] + 0.999)))
        else:
            text = self.t("hint_clicking")
        self.hint_label.configure(text=text)

    def refresh_points(self):
        shown = self.recording_points if self.state == RECORDING else self.points
        self.points_title.configure(text=f"{self.t('points_title')}  {len(shown)}/{MAX_POINTS}")
        for w in self.points_frame.winfo_children():
            w.destroy()
        if not shown:
            ctk.CTkLabel(self.points_frame, text=self.t("points_empty"), font=self.font(13, "bold"), text_color=TEXT_MUTED).pack(pady=(26, 2))
            ctk.CTkLabel(self.points_frame, text=self.t("points_empty_sub"), font=self.font(12), text_color=TEXT_MUTED, wraplength=380).pack()
            return
        can_delete = self.state == IDLE
        for i, (x, y) in enumerate(shown):
            r = ctk.CTkFrame(self.points_frame, corner_radius=10, fg_color=CARD_ALT)
            r.pack(fill="x", padx=4, pady=3)
            ctk.CTkLabel(
                r, text=str(i + 1), width=28, height=28, corner_radius=14, fg_color=ACCENT, text_color="#ffffff",
                font=self.font(12, "bold"),
            ).pack(side="left", padx=(8, 10), pady=6)
            ctk.CTkLabel(r, text=f"X {x}", font=ctk.CTkFont(family="Consolas" if IS_WIN else "monospace", size=13), width=90, anchor="w").pack(side="left")
            ctk.CTkLabel(r, text=f"Y {y}", font=ctk.CTkFont(family="Consolas" if IS_WIN else "monospace", size=13), width=90, anchor="w").pack(side="left")
            if can_delete:
                ctk.CTkButton(
                    r, text="✕", width=28, height=28, fg_color="transparent", text_color=TEXT_MUTED,
                    hover_color=("#fee2e2", "#7f1d1d"), font=self.font(13),
                    command=lambda i=i: self.delete_point(i),
                ).pack(side="right", padx=6)

    def refresh_stats(self):
        st = self.stats
        elapsed = time.monotonic() - st["start"] if self.state == CLICKING and st["start"] else st.get("elapsed", 0)
        self.stat_labels["clicks"].configure(text=str(st["clicks"]))
        self.stat_labels["cycles"].configure(text=str(st["cycles"]))
        self.stat_labels["time"].configure(text=format_elapsed(elapsed))

    def flash(self, text, seconds=4.0):
        self.message = text
        self.message_until = time.monotonic() + seconds
        self.message_label.configure(text=text)

    # ---------- input listeners (別スレッド) ----------
    def on_key_press(self, key):
        mod = modifier_of(key)
        if mod:
            self.pressed_mods.add(mod)
            return
        token = main_token(key)
        if token is None:
            return
        ident = (token, tuple(sorted(self.pressed_mods)))
        if ident in self.held_keys:
            return  # キーリピートは無視
        self.held_keys.add(ident)
        combo = make_combo(self.pressed_mods, token)
        if self.capturing is not None:
            self.events.put(("capture", combo))
            return
        for action, hk in self.hotkeys.items():
            if hk == combo:
                self.events.put(("hotkey", action))
                break

    def on_key_release(self, key):
        mod = modifier_of(key)
        if mod:
            self.pressed_mods.discard(mod)
            return
        token = main_token(key)
        self.held_keys = {h for h in self.held_keys if h[0] != token}

    def on_mouse_click(self, x, y, button, pressed):
        if pressed and button == mouse.Button.left and self.state == RECORDING:
            self.events.put(("click", (int(x), int(y))))

    # ---------- main-thread event loop ----------
    def poll(self):
        try:
            while True:
                try:
                    kind, value = self.events.get_nowait()
                except queue.Empty:
                    break
                if kind == "hotkey":
                    self.run_action(value)
                elif kind == "click":
                    self.handle_record_click(value)
                elif kind == "capture":
                    self.finish_capture(value)

            if self.state in (COUNTDOWN, CLICKING):
                if self.click_thread and not self.click_thread.is_alive():
                    finished = not self.stop_event.is_set()
                    self.stats["elapsed"] = time.monotonic() - self.stats["start"] if self.stats["start"] else 0
                    self.state = IDLE
                    self.refresh()
                    self.flash(self.t("msg_finished") if finished else self.t("msg_stopped"))
                else:
                    if self.state == COUNTDOWN and self.stats["countdown"] <= 0:
                        self.state = CLICKING
                        self.refresh()
                    elif self.state == COUNTDOWN:
                        self.update_hint()
                    self.refresh_stats()

            if self.message and time.monotonic() > self.message_until:
                self.message = ""
                self.message_label.configure(text="")
        except tk.TclError:
            return
        self.root.after(50, self.poll)

    def run_action(self, action):
        if action == "record_start":
            self.start_recording()
        elif action == "record_stop":
            self.stop_recording()
        elif action == "click_start":
            self.start_clicking()
        elif action == "click_stop":
            self.stop_clicking()

    # ---------- recording ----------
    def start_recording(self):
        if self.state != IDLE:
            return
        self.cancel_capture()
        self.commit_numbers()
        self.recording_points = []
        self.state = RECORDING
        self.refresh()

    def stop_recording(self):
        if self.state != RECORDING:
            return
        if self.recording_points:
            self.points = self.recording_points
            self.save()
            self.flash(self.t("msg_saved", n=len(self.points)))
        else:
            self.flash(self.t("msg_kept"))
        self.recording_points = []
        self.state = IDLE
        self.refresh()

    def handle_record_click(self, pos):
        if self.state != RECORDING:
            return
        x, y = pos
        left, top = self.root.winfo_rootx(), self.root.winfo_rooty()
        if left <= x < left + self.root.winfo_width() and top <= y < top + self.root.winfo_height():
            return  # このツール自身のウィンドウ上のクリックは記録しない
        if len(self.recording_points) >= MAX_POINTS:
            self.flash(self.t("msg_max", max=MAX_POINTS))
            return
        self.recording_points.append(pos)
        self.refresh_points()
        self.update_markers_for_state()
        if len(self.recording_points) >= MAX_POINTS:
            self.flash(self.t("msg_max", max=MAX_POINTS))

    # ---------- clicking ----------
    def start_clicking(self):
        if self.state != IDLE:
            return
        self.cancel_capture()
        self.commit_numbers()
        if not self.points:
            self.flash(self.t("msg_no_points"))
            return
        self.save()
        self.stop_event.clear()
        self.stats.update(clicks=0, cycles=0, start=time.monotonic(), countdown=float(self.delay), elapsed=0)
        self.state = COUNTDOWN if self.delay > 0 else CLICKING
        self.click_thread = threading.Thread(
            target=self.click_loop,
            args=(list(self.points), self.interval, self.button, self.repeat, self.delay),
            daemon=True,
        )
        self.click_thread.start()
        self.refresh()

    def stop_clicking(self):
        if self.state not in (COUNTDOWN, CLICKING):
            return
        self.stop_event.set()

    def click_loop(self, points, interval, button, repeat, delay):
        btn = mouse.Button.left if button == "left" else mouse.Button.right
        end = time.monotonic() + delay
        while True:
            remaining = end - time.monotonic()
            self.stats["countdown"] = max(0.0, remaining)
            if remaining <= 0:
                break
            if self.stop_event.wait(min(0.1, remaining)):
                return
        self.stats["start"] = time.monotonic()
        cycles = 0
        while not self.stop_event.is_set():
            for i, (x, y) in enumerate(points):
                if self.stop_event.is_set():
                    return
                self.mouse_ctl.position = (x, y)
                self.mouse_ctl.click(btn)
                self.stats["clicks"] += 1
                last = i == len(points) - 1 and repeat and cycles + 1 >= repeat
                if not last and self.stop_event.wait(interval):
                    return
            cycles += 1
            self.stats["cycles"] = cycles
            if repeat and cycles >= repeat:
                return

    # ---------- point list ----------
    def delete_point(self, index):
        if self.state != IDLE:
            self.flash(self.t("msg_busy"))
            return
        if 0 <= index < len(self.points):
            del self.points[index]
            self.save()
            self.refresh()

    def clear_points(self):
        if self.state != IDLE:
            return
        self.points = []
        self.save()
        self.flash(self.t("msg_cleared"))
        self.refresh()

    # ---------- markers ----------
    def show_markers(self, points):
        self.hide_markers()
        for i, (x, y) in enumerate(points, 1):
            self.markers.append(Marker(self.root, i, x, y))

    def hide_markers(self):
        for m in self.markers:
            m.destroy()
        self.markers = []

    def update_markers_for_state(self):
        if self.marker_job:
            self.root.after_cancel(self.marker_job)
            self.marker_job = None
        if self.state == RECORDING:
            self.show_markers(self.recording_points)
        else:
            self.hide_markers()

    def preview_markers(self):
        if self.state != IDLE or not self.points:
            return
        self.show_markers(self.points)
        self.marker_job = self.root.after(2500, self.hide_markers)

    # ---------- numeric settings ----------
    def set_interval(self, value):
        if self.state != IDLE:
            return
        self.interval_entry.delete(0, "end")
        self.interval_entry.insert(0, f"{value:g}")
        self.commit_numbers()

    def commit_numbers(self):
        if self.state != IDLE:
            return
        try:
            self.interval = clamp(float(self.interval_entry.get().replace(",", ".")), MIN_INTERVAL, MAX_INTERVAL)
        except ValueError:
            pass
        try:
            self.repeat = clamp(int(float(self.repeat_entry.get())), 0, 999999)
        except ValueError:
            pass
        self.interval_entry.delete(0, "end")
        self.interval_entry.insert(0, f"{self.interval:g}")
        self.repeat_entry.delete(0, "end")
        self.repeat_entry.insert(0, str(self.repeat))
        self.save()

    def on_delay_change(self, value, save=True):
        self.delay = int(round(value))
        self.delay_label.configure(text=f"{self.delay} {self.t('seconds')}")
        if save:
            self.save()

    def on_button_change(self, value):
        self.button = "left" if value == self.t("left") else "right"
        self.save()

    # ---------- hotkey capture ----------
    def begin_capture(self, action):
        if self.state != IDLE:
            self.flash(self.t("msg_busy"))
            return
        self.capturing = None if self.capturing == action else action
        self.held_keys.clear()
        self.refresh()

    def cancel_capture(self):
        if self.capturing is not None:
            self.capturing = None
            self.refresh()

    def finish_capture(self, combo):
        action = self.capturing
        if action is None:
            return
        if combo == "esc":
            self.capturing = None
            self.refresh()
            return
        for other, hk in self.hotkeys.items():
            if other != action and hk == combo:
                self.flash(self.t("msg_hk_dup", name=self.t("hk_" + other)))
                self.capturing = None
                self.refresh()
                return
        self.hotkeys[action] = combo
        self.capturing = None
        self.save()
        self.flash(self.t("msg_hk_set"))
        self.refresh()

    def reset_hotkey(self, action):
        default = DEFAULT_HOTKEYS[action]
        for other, hk in self.hotkeys.items():
            if other != action and hk == default:
                self.hotkeys[other] = DEFAULT_HOTKEYS[other]
        self.hotkeys[action] = default
        self.capturing = None
        self.save()
        self.refresh()

    def reset_all_hotkeys(self):
        self.hotkeys = dict(DEFAULT_HOTKEYS)
        self.capturing = None
        self.save()
        self.flash(self.t("msg_hk_set"))
        self.refresh()

    # ---------- appearance ----------
    def on_lang_change(self, value):
        for code, name in LANGS.items():
            if name == value and code != self.lang:
                self.lang = code
                self.capturing = None
                self.save()
                self.build_ui()
                return

    def on_theme_change(self, value):
        for key, name in self.theme_names.items():
            if name == value:
                self.theme = key
        ctk.set_appearance_mode(self.theme)
        self.save()

    def on_topmost_change(self):
        self.topmost = bool(self.topmost_switch.get())
        self.root.attributes("-topmost", self.topmost)
        self.save()

    # ---------- persistence ----------
    def load(self):
        try:
            with open(self.save_file, encoding="utf-8") as f:
                d = json.load(f)
        except (OSError, ValueError):
            return
        try:
            if d.get("lang") in LANGS:
                self.lang = d["lang"]
            if d.get("theme") in ("system", "light", "dark"):
                self.theme = d["theme"]
            self.points = [(int(x), int(y)) for x, y in d.get("points", [])][:MAX_POINTS]
            self.interval = clamp(float(d.get("interval", 1.0)), MIN_INTERVAL, MAX_INTERVAL)
            self.repeat = clamp(int(d.get("repeat", 0)), 0, 999999)
            self.delay = clamp(int(d.get("delay", 0)), 0, MAX_DELAY)
            self.button = "right" if d.get("button") == "right" else "left"
            self.topmost = bool(d.get("topmost", False))
            hk = d.get("hotkeys", {})
            merged = {a: (hk[a] if isinstance(hk.get(a), str) and hk[a] else DEFAULT_HOTKEYS[a]) for a in HOTKEY_ACTIONS}
            if len(set(merged.values())) == len(merged):
                self.hotkeys = merged
        except (TypeError, ValueError):
            pass

    def save(self):
        data = {
            "lang": self.lang,
            "theme": self.theme,
            "points": [list(p) for p in self.points],
            "interval": self.interval,
            "repeat": self.repeat,
            "delay": self.delay,
            "button": self.button,
            "topmost": self.topmost,
            "hotkeys": self.hotkeys,
        }
        try:
            with open(self.save_file, "w", encoding="utf-8") as f:
                json.dump(data, f, ensure_ascii=False, indent=2)
        except OSError:
            pass

    def on_close(self):
        self.stop_event.set()
        self.hide_markers()
        self.save()
        self.kb_listener.stop()
        self.mouse_listener.stop()
        self.root.destroy()


def main():
    root = ctk.CTk()
    ClickerApp(root)
    root.mainloop()


if __name__ == "__main__":
    main()

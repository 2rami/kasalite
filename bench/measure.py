#!/usr/bin/env python3
"""새 카사라이트 목표 수치(kasaterm docs/terminal-engine.md §4.3)를 잰다.

밖에서 잴 수 있는 것(크기·켜기·메모리·유휴 깨어남)은 어떤 앱이든 잰다. 옛 라이트 v0.1 과 새 라이트를
같은 틀로 재서 전후 표를 만든다. 키 지연·표시 박자는 앱 안 계측(`KASALITE_TRACE`, bench/README.md)이
있는 판에서만 나온다.

    python3 bench/measure.py --app ~/Applications/KasaLite.app --label old-v0.1 --out /tmp/x/old.json
    python3 bench/measure.py --app dist/KasaLite.app --dmg dist/KasaLite.dmg --label new --out /tmp/x/new.json
    python3 bench/measure.py --table /tmp/x/old.json /tmp/x/new.json

앱은 늘 격리 뿌리(`KASATERM_LITE_ROOT`·`KASALITE_ROOT`)로 띄워 사람이 쓰는 라이트 설정·세션을 건드리지 않고,
띄운 PID 만 거둔다.
"""

import argparse
import ctypes
import json
import os
import plistlib
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import Quartz

TARGETS = {
    "binary_mb": 15.0,
    "app_mb": 20.0,
    "dmg_mb": 10.0,
    "launch_ms": 150.0,
    "mem_1pane_mb": 90.0,
    "mem_10k_delta_mb": 10.0,
    "idle_wakeups_per_s": 0.0,
    "key_present_p50_ms": 3.0,
    "key_present_p95_ms": 6.0,
    "flood_fps": 115.0,
    "flood_interval_p99_ms": 12.0,
}


def mb(n):
    return round(n / 1e6, 1)


def du_bytes(path):
    out = subprocess.run(["du", "-sk", str(path)], capture_output=True, text=True, check=True).stdout
    return int(out.split()[0]) * 1024


def bundle_executable(app):
    info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    return app / "Contents/MacOS" / info["CFBundleExecutable"]


class Run:
    """격리 뿌리로 앱 하나를 띄우고, 잡아 둔 PID 만 거둔다."""

    def __init__(self, exe, extra_env=None):
        self.root = Path(tempfile.mkdtemp(prefix="klb-", dir="/tmp"))
        env = dict(os.environ)
        for k in list(env):
            if k.startswith(("KASATERM_", "CMUX_", "CLAUDE")) or k == "NO_COLOR":
                env.pop(k)
        env.update(
            KASATERM_LITE_ROOT=str(self.root / "lite"),
            KASALITE_ROOT=str(self.root / "lite"),
            KASATERM_NO_FOCUS="1",
            KASATERM_AUTOQUIT_MS="60000",
            TMPDIR=str(self.root) + "/",
        )
        env.update(extra_env or {})
        self.trace = self.root / "trace.jsonl"
        env.setdefault("KASALITE_TRACE", str(self.trace))
        self.log = open(self.root / "app.log", "w")
        self.t_spawn = time.time()
        self.proc = subprocess.Popen([str(exe)], env=env, stdout=self.log, stderr=subprocess.STDOUT, cwd=str(self.root))

    def first_window_ms(self, timeout=10.0):
        """창 서버에 이 PID 의 창이 화면에 올라온 시각. 첫 프레임보다 조금 이를 수 있다(빈 창)."""
        deadline = self.t_spawn + timeout
        while time.time() < deadline:
            wins = Quartz.CGWindowListCopyWindowInfo(Quartz.kCGWindowListOptionOnScreenOnly, Quartz.kCGNullWindowID)
            for w in wins or []:
                if w.get("kCGWindowOwnerPID") == self.proc.pid and w.get("kCGWindowAlpha", 0) > 0 and w.get("kCGWindowLayer", 1) == 0:
                    b = w.get("kCGWindowBounds", {})
                    if b.get("Width", 0) > 100:
                        return (time.time() - self.t_spawn) * 1e3
            time.sleep(0.002)
        return None

    def trace_events(self):
        if not self.trace.exists():
            return []
        out = []
        for line in self.trace.read_text().splitlines():
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                pass
        return out

    def footprint_mb(self):
        out = subprocess.run(["footprint", "-p", str(self.proc.pid)], capture_output=True, text=True).stdout
        for line in out.splitlines():
            # "kasaterm-lite [123]: 64-bit    Footprint: 61 MB (16384 bytes per page)"
            if "Footprint:" in line:
                val, unit = line.split("Footprint:")[1].split()[:2]
                return float(val) * {"KB": 1e-3, "MB": 1, "GB": 1e3}[unit]
        return None

    def idle_wakeups_per_s(self, seconds=6):
        """손대지 않은 동안 초당 깨어남(커널 rusage 의 interrupt + package idle wakeups). top 의 IDLEW 는 표본 사이에
        안 움직여 못 쓴다."""
        a = wakeups(self.proc.pid)
        time.sleep(seconds)
        b = wakeups(self.proc.pid)
        if a is None or b is None:
            return None
        return round((b - a) / seconds, 2)

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self.log.close()
        shutil.rmtree(self.root, ignore_errors=True)


class _RusageV2(ctypes.Structure):
    _fields_ = [
        ("uuid", ctypes.c_uint8 * 16),
        ("user_time", ctypes.c_uint64),
        ("system_time", ctypes.c_uint64),
        ("pkg_idle_wkups", ctypes.c_uint64),
        ("interrupt_wkups", ctypes.c_uint64),
        ("rest", ctypes.c_uint64 * 40),
    ]


def wakeups(pid):
    ru = _RusageV2()
    if ctypes.CDLL("/usr/lib/libproc.dylib").proc_pid_rusage(pid, 2, ctypes.byref(ru)) != 0:
        return None
    return ru.pkg_idle_wkups + ru.interrupt_wkups


def pct(v, p):
    v = sorted(v)
    return v[round((len(v) - 1) * p)] if v else None


def trace_metrics(ev):
    """bench/README.md 의 KASALITE_TRACE 줄들에서 키 지연·폭주 박자를 뽑는다."""
    m = {}
    t0 = next((e["wall"] for e in ev if e.get("ev") == "first_present"), None)
    if t0 is not None:
        m["first_present_wall"] = t0
    keys = [e for e in ev if e.get("ev") == "key"]
    pres = {e["seq"]: e for e in ev if e.get("ev") == "present"}
    shown = {e["seq"]: e["t"] for e in ev if e.get("ev") == "shown" and e.get("t", 0) > 0}
    kp = [(pres[k["seq"]]["t"] - k["t"]) * 1e3 for k in keys if k.get("seq") in pres]
    kg = [(shown[k["seq"]] - k["t"]) * 1e3 for k in keys if k.get("seq") in shown]
    if kp:
        m["key_present_p50_ms"] = round(pct(kp, 0.5), 2)
        m["key_present_p95_ms"] = round(pct(kp, 0.95), 2)
    if kg:
        m["key_glass_p50_ms"] = round(pct(kg, 0.5), 2)
        m["key_glass_p95_ms"] = round(pct(kg, 0.95), 2)
    flood = [e for e in ev if e.get("ev") == "flood"]
    if len(flood) == 2:
        lo, hi = flood[0]["t"], flood[1]["t"]
        ts = sorted(t for t in shown.values() if lo <= t <= hi)
        if len(ts) > 2:
            iv = [(b - a) * 1e3 for a, b in zip(ts, ts[1:])]
            m["flood_fps"] = round((len(ts) - 1) / (ts[-1] - ts[0]), 1)
            m["flood_interval_p99_ms"] = round(pct(iv, 0.99), 2)
    return m


def measure(args):
    app = Path(args.app).expanduser().resolve()
    exe = bundle_executable(app)
    r = {"label": args.label, "app": str(app), "measured_at": time.strftime("%Y-%m-%d %H:%M")}
    r["binary_mb"] = mb(exe.stat().st_size)
    r["app_mb"] = mb(du_bytes(app))
    if args.dmg:
        r["dmg_mb"] = mb(Path(args.dmg).expanduser().stat().st_size)

    # 켜기: 디스크 캐시가 데워진 뒤 값을 쓴다(첫 회는 버린다).
    launches, first_present = [], []
    for i in range(args.launches + 1):
        run = Run(exe)
        try:
            t = run.first_window_ms()
            time.sleep(0.6)
            m = trace_metrics(run.trace_events())
            if i > 0 and t is not None:
                launches.append(t)
                if "first_present_wall" in m:
                    first_present.append((m["first_present_wall"] - run.t_spawn) * 1e3)
        finally:
            run.close()
        time.sleep(0.5)
    r["launch_ms"] = round(statistics.median(launches), 1) if launches else None
    r["launch_samples_ms"] = [round(x, 1) for x in launches]
    if first_present:
        r["launch_first_present_ms"] = round(statistics.median(first_present), 1)

    # 메모리·유휴: 창 1·칸 1. 셸 프롬프트가 뜨고 가라앉은 뒤 잰다. 그다음 1만 줄을 흘려 스크롤백 증가분을 본다.
    fill = "seq 1 10000; clear"
    run = Run(exe, {"KASATERM_AUTOSEND": fill, "KASALITE_AUTOSEND": fill, "KASATERM_AUTOSEND_MS": "6000", "KASALITE_AUTOSEND_MS": "6000"})
    try:
        run.first_window_ms()
        time.sleep(3)
        r["mem_1pane_mb"] = run.footprint_mb()
        r["idle_wakeups_per_s"] = run.idle_wakeups_per_s()
        time.sleep(max(0.0, 6.0 + 3.0 - (time.time() - run.t_spawn)))
        r["mem_1pane_10k_mb"] = run.footprint_mb()
        if r["mem_1pane_mb"] and r["mem_1pane_10k_mb"]:
            r["mem_10k_delta_mb"] = round(r["mem_1pane_10k_mb"] - r["mem_1pane_mb"], 1)
    finally:
        run.close()

    # 키·폭주: 앱이 KASALITE_BENCH 를 알면 스스로 키를 넣고 출력을 흘린다(bench/README.md).
    run = Run(exe, {"KASALITE_BENCH": "keys=40,flood_ms=3000"})
    try:
        run.first_window_ms()
        time.sleep(args.bench_secs)
        r.update({k: v for k, v in trace_metrics(run.trace_events()).items() if k != "first_present_wall"})
    finally:
        run.close()
    return r


def table(paths):
    rows = [json.loads(Path(p).read_text()) for p in paths]
    keys = [k for k in TARGETS if any(k in r for r in rows)] + ["key_glass_p50_ms", "launch_first_present_ms", "mem_1pane_10k_mb"]
    head = "| 항목 | 목표 | " + " | ".join(r["label"] for r in rows) + " |"
    out = [head, "|" + "---|" * (2 + len(rows))]
    for k in keys:
        if not any(k in r for r in rows):
            continue
        goal = TARGETS.get(k)
        goal_s = "—" if goal is None else (f"≥ {goal:g}" if k == "flood_fps" else f"≤ {goal:g}")
        vals = []
        for r in rows:
            v = r.get(k)
            vals.append("—" if v is None else f"{v:g}")
        out.append(f"| {k} | {goal_s} | " + " | ".join(vals) + " |")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--app")
    ap.add_argument("--dmg")
    ap.add_argument("--label", default="app")
    ap.add_argument("--out")
    ap.add_argument("--launches", type=int, default=5)
    ap.add_argument("--bench-secs", type=float, default=12.0)
    ap.add_argument("--table", nargs="+")
    args = ap.parse_args()
    if args.table:
        print(table(args.table))
        return
    if not args.app:
        ap.error("--app 또는 --table")
    r = measure(args)
    text = json.dumps(r, ensure_ascii=False, indent=2)
    if args.out:
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(text)
    print(text)


if __name__ == "__main__":
    sys.exit(main())

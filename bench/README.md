# 목표 수치 재기

새 카사라이트가 kasaterm `docs/terminal-engine.md` §4.3 의 목표에 닿았는지, 옛 라이트(v0.1)와 같은 틀로 잰다.

```bash
python3 bench/measure.py --app ~/Applications/KasaLite.app --label v0.1 --out /tmp/klb/v0.1.json
python3 bench/measure.py --app dist/KasaLite.app --dmg dist/KasaLite.dmg --label v0.2 --out /tmp/klb/v0.2.json
python3 bench/measure.py --table /tmp/klb/v0.1.json /tmp/klb/v0.2.json   # 마크다운 전후 표
```

앱은 매번 `/tmp/klb-*` 격리 뿌리로 띄우고(`KASATERM_LITE_ROOT`·`KASALITE_ROOT`·`TMPDIR`), 띄운 PID 만 거둔다.
사람이 쓰는 라이트의 설정·세션은 건드리지 않는다. 한 번에 앱 하나만 뜬다.

## 밖에서 재는 것 — 어떤 판이든

| 항목 | 재는 법 |
|---|---|
| `binary_mb` | 번들 실행 파일 크기 |
| `app_mb`·`dmg_mb` | `du -sk` · 파일 크기 |
| `launch_ms` | 실행 → 그 PID 의 창이 창 서버에 화면으로 올라온 시각(CGWindowList 2ms 폴링). 첫 회는 버리고 5회 중앙값 |
| `mem_1pane_mb` | 창 1·칸 1, 프롬프트가 뜨고 3초 뒤 `footprint` |
| `mem_10k_delta_mb` | 같은 칸에 `seq 1 10000` 을 흘린 뒤 `footprint` 증가분 |
| `idle_wakeups_per_s` | 손대지 않은 6초 동안 커널 rusage(`proc_pid_rusage`)의 interrupt + package idle 깨어남. `top` 의 IDLEW 는 표본 사이에 안 움직여 못 쓴다 |

## 앱 안 계측 — 새 판이 지켜야 할 계약

키 지연·표시 박자는 밖에서 못 잰다(이 맥 세션엔 화면 녹화 권한이 없고, Typometer 도 화면을 찍어야 한다).
그래서 앱이 아래 환경 변수를 알면 스스로 기록한다. 시각은 모두 `CACurrentMediaTime`(초) 한 시계로 쓴다 —
Metal 의 `presentedTime` 이 같은 시계라 화면에 뜬 시각과 바로 뺄 수 있다.

**`KASALITE_TRACE=<경로>`** — 한 줄에 JSON 하나를 덧붙인다.

| `ev` | 필드 | 언제 |
|---|---|---|
| `first_present` | `wall`(유닉스 초) | 첫 프레임을 낸 직후 한 번 |
| `key` | `t`, `seq` | 키 이벤트를 받은 순간. `seq` 는 그 키를 반영해 낸 프레임 번호 |
| `present` | `t`, `seq` | `present()` 가 돌아온 순간 |
| `shown` | `t`, `seq` | 그 프레임 drawable 의 presented 손잡이(`presentedTime`, 버려졌으면 0) |
| `flood` | `t` | 출력 폭주 구간의 시작과 끝에 한 번씩 |

**`KASALITE_BENCH=keys=40,flood_ms=3000`** — 켜진 뒤 1초 쉬고, 초점 칸에 `flood_ms` 동안 색 섞인 출력을 흘린다
(`flood` 줄로 앞뒤를 표시). 2초 쉰 뒤 키 `keys` 개를 150ms(+0~8ms 흩뜨림 — 박자와 같은 자리에 늘 떨어지지 않게) 간격으로 자기 뷰에 넣는다(macOS 는 NSEvent keyDown 을
winit 뷰에 직접 — 창 서버를 안 거쳐 사람 초점을 안 뺏는다). 끝나면 스스로 닫힌다.

**`KASALITE_AUTOSEND=<글>`·`KASALITE_AUTOSEND_MS`** — 초점 칸에 글을 넣고 줄을 끝낸다(옛 `KASATERM_AUTOSEND` 와 같음).

이 계약의 표본 구현은 `spikes/frame-pacing` 이다. 거기서 `presentedTime` 을 받는 법(`nextDrawable` 감싸기)과
합성 키 넣는 법을 그대로 옮긴다.

| 항목 | 목표 |
|---|---|
| `key_present_p50_ms` / `p95` | ≤ 3 / ≤ 6 |
| `flood_fps` | 120Hz 화면에서 ≥ 115 |
| `flood_interval_p99_ms` | ≤ 12 |

`key_glass_*`(키 → 화면에 뜬 시각)도 함께 낸다. 목표 표의 Typometer 칸(평균 ≤ 8ms)에 가장 가까운 값이다.

## 주의

- **다른 일이 돌면 박자가 무너진다.** 2026-10-06 실측에서 같은 설정이 120fps 와 50fps 사이를 오갔다 — 다른 칸의
  `rustc`·시뮬레이터·창 서버 경합 때문이다. 표에 넣는 값은 `ps -Ao pcpu -r | head` 로 큰 일이 없는 걸 보고 잰다.
- 창이 다른 창에 가리면 `shown` 이 안 온다(가려진 창은 표시되지 않는다). 측정 창은 화면 구석에 띄운다.
- `launch_ms` 는 빈 창이 먼저 올라오는 판이면 첫 프레임보다 이르다. 새 판은 `launch_first_present_ms`(trace)로 함께 본다.

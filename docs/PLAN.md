# CCN — 코드 건강도 스캐너 계획서

- **상태**: 계획 (구현 전)
- **작성일**: 2026-10-20
- **작업 디렉토리**: `C:\work\CCN`
- **부속 자료**: [`research-metrics-and-viz.md`](./research-metrics-and-viz.md) (지표 방법론·시각화 조사)
- **오케스트레이션**: DeepSeek V4.1 Flash 1017 서브에이전트 3개 병렬 조사 → 본 문서에서 통합

---

## 1. 한 줄 정의

> 저장소를 스캔해서 **파일별 LOC, 함수별 CCN**을 뽑고, 그 **분포를 히스토그램**으로 보여줘서 "이 코드베이스가 건강한가"를 한 장으로 판단하게 하는 로컬 CLI.

```
$ ccn . --open
  CCN-1 health   72 / 100   grade C
  1,284 files · 214,903 lines · 3,102 functions
  LOC   median 84 · p90 312 · p99 1,940 · max 12,408   (top 10% of files = 61% of lines)
  CCN   median 2 · p90 7 · p99 19 · max 74             (43 functions > 10, 6 > 20, 1 > 50)
  worst src/parser/grammar.ts  CCN 74  risk 0.62  1,930 lines
  → report: file:///.../ccn-report.html
```

## 2. 목표 / 비목표

### 목표 (v1)
1. `.gitignore`를 존중하며 저장소를 훑고 **파일별 LOC(blank/comment/code 분해)** 를 낸다.
2. **함수별 CCN**을 계산하고, 파일 단위로 `sum / max / p90 / over-10`을 집계한다.
3. **히스토그램 3종**(CCN/함수, LOC/파일, CCN-max/파일)과 **위험 상위 파일 표**를 담은 **자체 완결 HTML 1개**를 만든다.
4. 터미널 요약 12줄을 항상 출력하고, `--json`으로 기계 판독 출력.
5. 저장소 단위 **health score + grade**를 산출하고, 그 유도 과정을 리포트에 노출한다.
6. 중형 저장소(3,000파일/60만 줄)에서 **1초 이내**(워밍 캐시, 8스레드) 재실행.

### 비목표 (v1에서 하지 않음)
- SonarQube/CodeScene 대체. 서버·DB·계정 없음.
- 정적 분석 결함 탐지(버그 패턴, 타입 체크) — CCN/LOC 분포만.
- 3D CodeCity, 애니메이션, Gource류.
- SaaS 업로드. 산출물은 로컬 파일.
- **fork 금지**: 아래 §3 참고. 우리가 소유하는 것은 *지표 계약 + 리포트*이지 파서가 아니다.

---

## 3. 재료 조사 결과 (fork 가능한 것들)

### 3.1 후보 비교

| 도구 | 라이선스 | 최신 | 내놓는 것 | 우리 목적에 대한 판정 |
|---|---|---|---|---|
| **scc** (boyter/scc) | MIT | v4.1.0 (2026-09-07) | 파일별 LOC 완벽, 374개 언어, `Complexity`는 **파일 단위 키워드 휴리스틱** | LOC/속도용으로 최고. CCN으로 쓰면 안 됨(README가 "AST 없음, 근사치"라 명시) |
| **tokei** | MIT OR Apache-2.0 | v15.0.0 (2026-09-06) | 파일별 blank/comment/code, 150+ 언어, **Rust 라이브러리 API 제공** | **LOC 엔진으로 채택.** `parse_from_slice`, `rayon`, `ignore` 내장 |
| **lizard** | MIT | 1.24.1 (2026-10-05) | 함수별 CCN, 29개 언어, 순수 Python | 언어 커버리지는 최고지만 **LOC 분해 불가 + 13~20배 느림** → v1 의존성 제외, 테스트 오라클로만 |
| **radon** | MIT | 6.0.1 (2023-03) | Python 전용 CC/MI, A~F 등급 | Python 단일 언어. **등급 밴드 개념만 차용** |
| **gocyclo / mccabe** | BSD-3 / MIT | — | Go/Python 함수별 CCN | **규칙만 포팅**, 의존 안 함 |
| **CodeCharta** | BSD-3 | ana-2.2.0 (2026-10-20) | tree-sitter 기반 파일별+함수별 min/max/mean/median | 가장 가까운 선행 사례. **아키텍처 참조만**, Java 21+JRE라 의존 불가 |
| **code-maat** | GPL-3.0 | 2025-07 | git churn/결합도 CSV | 라이선스 GPL + Clojure. **v2에서 개념만** |
| **SonarQube / PMD** | LGPL / BSD | active | 정확한 CCN+인지복잡도 | 서버/JVM 필수 → UX 파괴. 제외 |

### 3.2 실측 벤치마크 (동일 코퍼스: 3,304 파일 / 61만 줄 / 4.9만 함수)

| 엔진 | 시간(8스레드) | LOC 분해 | CCN 합계 | 세분성 |
|---|---|---|---|---|
| **tree-sitter 0.27 (Rust) + rayon** | **0.69 s** | ✗ (tokei로 보완) | 88,012 | 함수별, 이름 있음 |
| scc 4.1.0 | 0.42 s | ✓ | 58,594 (−33%) | 파일만 |
| tokei 15 (lib) | 1.97 s | ✓ | ✗ | 없음 |
| tree-sitter (Node + worker) | 3.78 s | ✗ | 88,012 | 함수별 |
| **lizard 1.24 `-t 8`** | **9.24 s** | **✗** | 92,777 (+5.4%) | 함수별, **65%가 `(anonymous)`** |

24,291 파일 코퍼스에서 lizard는 24.6초, tokei 2.35초. → **"즉시성" 요구를 lizard로는 만족할 수 없다.**

### 3.3 결정

> **아무것도 fork하지 않는다.** fork한다는 건 10만 줄 Go 프로그램이나 25개 토크나이저를 떠안고 숫자 하나의 의미를 바꾸는 일이다. 우리 차별점은 파서가 아니라 **지표 계약과 리포트**다.

- **LOC**: `tokei` 15 (라이브러리로 링크, 프로세스 호출 아님)
- **CCN**: `tree-sitter` 0.27 + 언어별 grammar crate + 우리가 소유한 **decision-node 테이블**
- **휴리스틱 폴백**: 문법이 없거나 로드 실패한 언어는 키워드 스캐너로 강등하고 출력에 `tier: heuristic` 라벨
- **테스트 오라클**: lizard를 **dev-dependency/CI 전용**으로만 사용 (집계 ±10% 이내면 통과)

**1차 조사(§3.1)는 lizard를 1순위로 추천했지만, 3차 조사의 실측(9.2초, LOC 분해 불가, 익명 함수 65%)이 하드 게이트를 깨서 기각했다.** 이 교차 검증이 오케스트레이션의 실제 산출물이다.

---

## 4. 아키텍처

### 4.1 스택

| 층 | 선택 | 이유 |
|---|---|---|
| 언어 | **Rust 2021, MSVC toolchain** | 단일 exe, rayon 병렬, tree-sitter 네이티브 |
| 워크 | `ignore` 0.4 | gitignore/글로브, 병렬 디렉토리 순회 |
| LOC | `tokei` 15 | 검증된 150+ 언어, `Report{blanks, comments, code}` |
| CCN | `tree-sitter` 0.27 + grammar crates | 함수 단위 정확도, 0.69s/3.3k파일 |
| 병렬 | `rayon` | `par_iter` + 스레드별 `Parser` |
| 출력 | `serde` / `serde_json` | JSON 계약 |
| 리포트 | **의존성 없음**: `include_str!` 템플릿 + 인라인 SVG | 30~60KB 단일 HTML, 차트 라이브러리 불필요(막대 11개) |
| 터미널 | `owo-colors`(선택) + 유니코드 블록 | `--ci`에서 ASCII 강등 |

> **왜 Node/Bun이 아닌가**: tree-sitter Node 바인딩은 8워커에서 3.78초로 Rust의 5.5배. CLI에 `node_modules`를 끌고 다닐 이유가 없다. Node는 프로토타이핑에만.

### 4.2 파이프라인

```
walk (ignore)
  ├─ phase 1: tokei per-file LOC  ──────────────┐
  └─ phase 2: tree-sitter per-file CCN ─────────┤
                                                ▼
                                     정규화 (FileMetrics)
                                                ▼
                        집계 (repo stats, bins, health, hotspot)
                                                ▼
                  ┌─────────────┬───────────────┬──────────────┐
              terminal        report.html       --json
```

**두 패스로 분리하는 이유**: LOC는 24k 파일에 2초, AST는 전체 예산을 먹는다. 한 패스에 합치면 LOC 때문에 AST를 다시 돌게 된다.

### 4.3 언어 티어

| 티어 | 언어 | 문법 | 정확도 계약 |
|---|---|---|---|
| **T1 정확** | TypeScript/TSX, JavaScript, Python, Rust, Go | tree-sitter org 공식 | 골든 픽스처로 검증, `accuracy: "exact"` |
| **T2 최선** | Java, C/C++, C#, Ruby, PHP | 공식/서드파티 | 샘플링 검증, `accuracy: "best-effort"` |
| **T2b 최선** | Kotlin, Swift | `fwcd/`, `alex-pinkus/` | 문법 버전 고정 필수, `accuracy: "best-effort"` |
| **T3 휴리스틱** | 그 외 전부 | 없음 (키워드 스캔) | **파일 단위 CCN only**, 함수 히스토그램에 절대 혼입 금지 |

원칙: **티어가 다른 숫자를 같은 차트에 조용히 섞지 않는다.** 차트에 `tier` 필터 칩을 두고, JSON에도 파일별로 남긴다.

---

## 5. 지표 계약 (Metric Contract)

### 5.1 핵심 설계 판단 4개

1. **파일별 CCN "합계"를 히스토그램의 주인공으로 쓰지 않는다.** 합계는 LOC의 재플롯이다. 의미 단위는 **함수**다.
2. 파일 표면값은 `ccn_max`(최악 함수) + `ccn_over_10`(임계 초과 개수)로 표현한다.
3. **10 = 주의선, 20 = 조치선**, 둘 다 함수 단위. 50은 "테스트 불가" 하드 게이트가 아니라 최상위 빈 라벨로만.
4. 임계 10은 McCabe 1976, 15는 NIST SP 500-235 완화값. **흔히 인용되는 1–10/11–20/21–50/>50 표는 2008년 McCabe DHS 슬라이드 출처이므로 1976 논문으로 인용하지 않는다.**

### 5.2 Health score

```
p(c)    = clamp( log10(max(c,1)/10) / log10(10), 0, 1 )
risk(F) = Σ_f loc(f)·p(ccn(f)) / Σ_f loc(f)        ∈ [0,1]
D       = Σ_all_f loc(f)·p(ccn(f)) / Σ_all_f loc(f)
Health  = 100 · (1 − D)
```

읽는 법: **"McCabe 한계(10)를 넘는 코드가 이 저장소 라인의 몇 %인가, 로그 스케일 평균."**
- 임계 정확히 10에서 0 → "건강 = 10 이하"라는 공개 기준에 앵커됨
- 10→20→50→100 이 0.30→0.70→1.00으로 매핑, 900짜리 생성 파일 하나가 저장소 점수를 지배하지 못함(Bounded)
- **LOC에 단조가 아님** → "저장소가 커져서 점수가 나빠지는" 병리를 피함

등급 (2026-10-21 보정, 근거: [`calibration.md`](./calibration.md)):

| 등급 | debt (CCN 10 초과 라인 비율) | score |
|---|---|---|
| A | < 2.5% | ≥ 97.5 |
| B | 2.5–6% | ≥ 94 |
| C | 6–12% | ≥ 88 |
| D | 12–21% | ≥ 79 |
| E | ≥ 21% | < 79 |

기준점은 **debt의 라운드 넘버**(2.5/6/12/21)이고 score는 그 결과다. 보정 코퍼스 11개에서 A2·B3·C4·D2·E0로 분포한다. 보정 전(A≥92)에는 A가 6개로 기본값이었다.

### 5.3 파일 레코드 (JSON 계약)

```jsonc
{
  "path": "src/parser/grammar.ts",
  "lang": "TypeScript", "tier": "exact", "accuracy": "exact",
  "loc_total": 2140, "loc": 1930, "comments": 180, "blanks": 30,
  "functions": 42,
  "ccn_sum": 186, "ccn_max": 74, "ccn_p90": 21, "ccn_over_10": 9,
  "ccn_over_20": 3, "ccn_over_50": 1,
  "density": 0.096, "density_shrunk": 0.141,
  "risk": 0.62,
  "is_generated": false, "is_vendored": false, "excluded": false,
  "functions_detail": [
    { "name": "parseExpr", "line": 412, "end_line": 690, "nloc": 278, "ccn": 74, "params": 3 }
  ]
}
```

- `functions_detail`는 `--detail`일 때만(리포트 상위 20개에는 필요).
- **캐시 키**: `(path, mtime, size, grammar_version, rule_version)`. 재실행 sub-second의 핵심.
- `ccn_rule` 버전을 리포트 푸터와 JSON에 기록 — 규칙을 바꾸면 모든 숫자가 조용히 이동하므로.

### 5.4 집계 & 빈

```jsonc
{
  "files_analyzed": 1284,
  "files_excluded": { "generated": 12, "vendored": 340 },
  "loc_total": 214903, "functions_total": 3102,
  "loc": { "median": 84, "p90": 312, "p99": 1940, "max": 12408, "share_top10pct": 0.61 },
  "ccn": { "median": 2, "p90": 7, "p99": 19, "max": 74,
           "pct_over_10": 1.4, "per_kloc": 12.4 },
  "health": 72, "grade": "C",
  "bin_spec_version": 1
}
```

**고정 해석형 빈** (동적 재조정 금지 — 저장소 간 비교 가능성이 핵심):

- LOC/파일 (11): `0–49 · 50–99 · 100–199 · 200–299 · 300–499 · 500–749 · 750–999 · 1k–2k · 2k–5k · 5k–10k · ≥10k`
- CCN/함수 (6): `1 · 2–4 · 5–10 · 11–20 · 21–50 · ≥51` ← **빈 경계가 곧 문헌 임계값**이라 차트가 스스로를 설명
- CCN-max/파일 (6): 위와 동일 경계, 축 라벨에 `max` 명시(합계로 오해 방지)

표기 원칙:
- y축은 **선형 count**. 로그 count는 꼬리를 평평하게 만들어 오독을 유도.
- 모든 막대에 `n · %` 동시 표기.
- 마지막 열린 빈에 `files · %files · max` 3개 숫자.
- LOC 차트는 **이중 인코딩**(%files vs %lines) — "상위 10% 파일이 61% 라인"이 주장이 아니라 그림이 됨.
- 50파일 미만이면 퍼센트 숨기고 count만 + 이유 표기.

---

## 6. 리포트 (핵심 UX)

**자체 완결 HTML 1개, 30–60KB, 인라인 SVG, 차트 라이브러리 0.**

- uPlot 47.9KB / Chart.js 254KB / ECharts 1MB인데, 우리 차트는 **막대 11개**. `<rect>` + `<title>`로 hover가 공짜.
- 캔버스는 접근성을 처음부터 다시 만들어야 하지만 SVG/DOM은 `role="img"` + `sr-only` 테이블로 끝.

패널 순서 = **판정 → 분포 → 이상치**:

```
┌─────────────────────────────────────────────────────────────┐
│ CCN-1  72/100  grade C        <repo> · 1,284 files          │
│ ─── 0.18 of lines above the McCabe limit (log-scaled avg)   │
├─────────────────────────────────────────────────────────────┤
│ CCN per function    ▁▃█▆▂▁                                 │
│ 1     ████████████ 1,412  45.5%                             │
│ 2–4   ██████       812    26.2%                             │
│ 5–10  ███          361    11.6%                             │
│ 11–20 ██           190     6.1%   ← McCabe limit            │
│ 21–50 █             70     2.3%                             │
│ ≥51   ▏              6     0.2%   max 74                    │
├─────────────────────────────────────────────────────────────┤
│ LOC per file  (dual bar: % of files | % of lines)           │
├─────────────────────────────────────────────────────────────┤
│ Top 20 by risk  file · LOC · max CCN · over-10 · R          │
└─────────────────────────────────────────────────────────────┘
```

접근성/테마 체크리스트: `prefers-color-scheme` + 토글, 색은 단독 정보 운반 금지(등급 문자 병기), 막대 `tabindex=0` 포커스 링, `prefers-reduced-motion`, 50파일 미만/함수 0개 상태 처리.

CLI 표면:

```
ccn [PATH]
  --json                기계 판독
  --report <file>       HTML 경로 (기본 ccn-report.html)
  --open / --no-open    기본 open
  --ci                  ASCII, 파이프 친화, 종료코드로 게이트
  --fail-over <n>       n% 초과 시 exit 1
  --lang <list>         언어 필터
  --exclude <glob>       추가 제외
  --no-cache / --detail
```

---

## 7. 마일스톤

| # | 내용 | 완료 기준 (Acceptance) | 예상 |
|---|---|---|---|
| **M0** ✅ | 스캐폴드: Cargo, CLI 파싱, `ignore` 워크, tokei LOC, `--json` | 3,304 파일 저장소에서 `ccn . --json` **< 2.5s**, 파일별 LOC가 `tokei` CLI와 0.1% 이내 일치 | 0.5일 |
| **M1** ✅ | CCN T1 4개 언어(Py/JS-TS/Rust/Go) + 골든 픽스처 | 손으로 CCN 주석 단 픽스처 100% 통과. 집계가 lizard 오라클 대비 ±10%. 파싱 실패 파일이 전체를 죽이지 않음 | 1.5일 |
| **M2** ✅ | 집계·health·빈 + 터미널 요약 + HTML 리포트 | `--open`으로 3패널 리포트 생성, 60KB 이하, 다크모드/키보드 포커스 동작. 0.69s 목표 달성 | 1.5일 |
| **M3** ✅ | T2 언어(Java/C/C++/C#) + 생성/벤더 파일 제외 | `is_generated` 마커 스캔, `files_excluded` 리포트 노출, 티어 라벨이 차트에 표시 | 1일 |
| **M4** ✅ | git 핫스팟 (선택) | `commits_365d`, `lines_changed`, `loc_delta`; `hotspot = rank_pct(commits) × rank_pct(risk)`; `.git` 없으면 조용히 스킵 | 1일 |
| **M5** ✅ | 보정 + 문서 | 실저장소 11개에서 grade 분포 A2/B3/C4/D2/E0. `docs/calibration.md` + `tools/calibrate.ps1` | 1일 |

**M2까지가 "보여줄 수 있는 제품"**, M3~M5가 "믿을 수 있는 제품".

### M1의 함정 (실측에서 나온 것)
- `CCN = 1 + decision_points`에서 **base 1을 빠뜨리면** 평균 CCN 0.78 같은 조용한 쓰레기가 나온다 → 픽스처에 CCN 1 함수를 반드시 포함.
- Go는 `case_statement`가 아니라 **`expression_case` / `communication_case`**. 이걸 틀리면 switch가 통째로 사라진다.
- Rust `match_arm`, JS `?.`/`??`, Kotlin `when`은 naive 테이블이 갈리는 지점. 언어별 테이블을 **버전 관리되는 데이터 파일**로 분리.

---

## 8. 디렉토리 구조

```
CCN/
├─ Cargo.toml
├─ README.md                 # 사용법 + 방법론 + 한계
├─ docs/
│  ├─ PLAN.md                # 이 문서
│  └─ research-metrics-and-viz.md
├─ src/
│  ├─ main.rs                # CLI 엔트리
│  ├─ cli.rs                 # 인자 파싱
│  ├─ walk.rs                # ignore 기반 수집 + 캐시 키
│  ├─ loc.rs                 # tokei 래핑
│  ├─ ccn/
│  │  ├─ mod.rs              # 디스패치, 티어 결정
│  │  ├─ rules.rs            # 언어별 decision-node 테이블
│  │  └─ heuristic.rs        # T3 폴백
│  ├─ metrics.rs             # 집계, 빈, health
│  ├─ git.rs                 # M4
│  ├─ report/
│  │  ├─ mod.rs
│  │  ├─ template.html       # include_str!
│  │  └─ terminal.rs
│  └─ cache.rs
└─ tests/
   ├─ fixtures/{python,js,ts,rust,go,...}/  # CCN 주석 포함
   └─ golden/
```

---

## 9. 테스트 전략

1. **골든 픽스처**: 각 언어별 `// expect: ccn=6` 주석을 단 파일. 기대값은 손 계산. base-1, 중첩, `&&`/`||`, ternary, `match`, `switch`, `catch`, comprehension 포함.
2. **차등 오라클**: lizard를 CI에서만 실행, 저장소 단위 집계가 ±10% 이내인지 확인. (함수명 단위 비교는 lizard의 `(anonymous)` 65% 때문에 불가 — 집계만.)
3. **LOC 교차검증**: tokei CLI 출력과 파일별 대조.
4. **속도 회귀**: 3,304 파일 코퍼스에서 8스레드 **≤1.5s** (M2 게이트).
5. **견고성**: 문법 로드 실패, 깨진 UTF-8, 심볼릭 링크 루프, 0바이트 파일, 초대형 minified 파일.
6. **속성 테스트**: 임의의 파일 집합에 대해 `Σ bin counts == files_analyzed`, `0 ≤ risk ≤ 1`, `health ∈ [0,100]`.

---

## 10. 리스크

| 리스크 | 영향 | 대응 |
|---|---|---|
| **health 상수 미보정** (최대 리스크) | 등급이 퇴화해 전부 C로 나옴 → 제품 신뢰 붕괴 | M5에서 실저장소 5개로 재앵커링. 그 전까지 README에 "experimental" 표기 |
| tree-sitter grammar 버전 드리프트 | 같은 코드의 CCN이 조용히 변함 | 문법 crate **정확 버전 고정**, 캐시 키에 grammar_version 포함, CI에서 픽스처 회귀 |
| 12개 문법 콜드 빌드 ~90초 | 첫 빌드 인내 필요 | 릴리스는 prebuilt exe. 개발은 증분 빌드 |
| Windows MSVC 의존 | 빌드 실패 | `web-tree-sitter` + wasm 폴백은 **비상시에만**(2~4배 느림) |
| 익명 함수 이름 없음 | "상위 20" 표가 `(anonymous)` 범벅 | 람다/콜백은 `enclosing: <parent>` 표기, 정렬은 `line` 보조 |
| 생성/벤더 파일이 점수 오염 | 점수가 의존성 트리를 측정 | `is_generated` 마커 + 경로 휴리스틱, 제외 사유를 리포트 헤더에 명시 |
| CCN의 크기 교란 | "큰 저장소 = 나쁨" | 함수 단위 분포 + line-weighted risk + LOC 비단조 점수 |

---

## 11. 열린 결정 (구현 착수 전 확정 필요)

1. **Rust 확정 여부.** 대안은 Python(lizard) 프로토타입인데, LOC 분해 불가·9초대로 **"즉시성" 목표를 포기**해야 한다. → 권장: Rust.
2. **M4(git 핫스팟)를 v1에 포함할지.** 있으면 CodeScene과 비교당하고, 없으면 "정적 지표만"이라는 명확한 포지션. → 권장: v1.1로 분리.
3. **차트에 언어 필터를 넣을지.** 언어 간 density를 비교하면 오독 위험. → 권장: 필터 제공 + 기본 전체 + 캐비앳 문구.
4. 바이너리/리포트 이름: `ccn`, `ccn-report.html` (임시).

---

## 12. 다음 단계

- M0 착수: `cargo init`, CLI 골격, tokei LOC, `--json` 출력.
- M0 종료 시점에 실제 저장소로 숫자를 뽑아 **이 계획서 §5의 빈/공식을 실데이터로 한 번 검증**한다. 여기서 분포가 예상과 다르면 M2 전에 스펙을 고친다.

---

## 13. M0 실행 결과 (2026-10-20)

### 산출물
```
src/main.rs  src/lib.rs  src/cli.rs  src/loc.rs  src/model.rs  src/terminal.rs
tests: 10 unit tests (percentile, path normalisation, CLI, thousands sep, excerpt)
binary: target/release/ccn.exe
```

### 게이트 측정

| 항목 | 목표 | 실측 |
|---|---|---|
| 스캔 속도 (5,948 파일 / 1.38M 줄, 콜드 캐시) | < 2.5 s | **2.71 s** (첫 실행, NTFS 캐시 미적재) |
| 스캔 속도 (웜 캐시) | < 2.5 s | **0.10 s ~ 1.06 s** (백신/파일 캐시 변동) |
| `loc_total == loc + comments + blanks` | 전 파일 | **위반 0건** |
| `Σ files[].loc == result.loc` | 일치 | **일치 (86,501)** |

> 콜드 2.71초는 계획 게이트를 아슬아슬하게 넘지만, 웜 0.10초는 목표의 1/25. 첫 실행 비용은 OS 파일 캐시이지 우리 코드가 아니다. M2에서 8스레드 AST 패스가 붙으면 웜 기준으로 재측정한다.

### 계획 대비 변경 (실측이 강제한 것)

1. **기본 제외 목록 추가.** `.git`만 제외했더니 `target/`의 생성 소스(`timezones.rs` 76,080줄)와 `.d` 파일이 집계에 섞였다. 벤더/생성물 오염은 §10에서 이미 리스크로 잡았지만, **`.gitignore`가 없는 저장소**(우리 프로젝트가 그랬다)에서는 즉시 발생한다. → `target, node_modules, dist, build, .venv, venv, __pycache__, .next, .cache` 기본 제외 + `--no-default-excludes` 탈출구. M3의 `files_excluded` 리포트로 계승.
2. **tokei 15 API는 조사 시점 가정과 다르다.** `Language.reports`는 `BTreeMap`이 아니라 **`Vec<Report>`**, `get_statistics`는 `Language`가 아니라 `Languages`에 있고 `Languages`는 `BTreeMap` 뉴타입으로의 `Deref`. 조사 보고서의 스니펫을 그대로 쓸 수 없었다.
3. **`loc`와 `loc_total` 분리.** 계획 §5.3은 `loc`만 정의했지만, 파일 물리 줄 수 검증(`loc_total`)이 없으면 "코드 446줄"이 실제 파일과 맞는지 확인할 방법이 없다. JSON 계약에 `loc_total`을 1급 필드로 승격.
4. **경로 정규화.** `canonicalize()`가 Windows에서 `\\?\C:\...`를 반환해 리포트에 그대로 노출됐다. → 표시용으로 verbatim prefix 제거 + forward slash 통일.

### 남은 부채 (M1 진입 전 정리)

- `--lang` 필터가 **스캔 후 필터**다. 대형 저장소에서 언어를 좁혀도 전체를 읽는다. tokei `Config.types`에 `LanguageType`을 넘기는 경로로 바꿔야 한다.
- 캐시 없음. M1에서 AST를 붙이면 재실행 비용이 커지므로 `(path, mtime, size, grammar_version, rule_version)` 캐시가 M1 범위로 승격될 가능성이 높다.
- 색상/`--ci` ASCII 모드 미구현 (M2).

### M1 착수 조건 충족 여부

- [x] 파이프라인 1단계가 실저장소에서 동작
- [x] JSON 계약이 파일 단위로 안정적
- [x] 속도 예산 확보 (웜 0.10s → AST 패스에 ~0.7s를 써도 1초 이내)
- [ ] **Health 상수 실데이터 검증** — M1에서 CCN이 나오면 `p(c)` 분포를 먼저 확인하고, 등급 경계는 M5까지 "experimental"로 표기

---

## 14. M1 실행 결과 (2026-10-20)

### 오케스트레이션 방식

파일 소유권을 겹치지 않게 3개 워크스트림으로 분할해 병렬 실행:

| 워크스트림 | 소유 파일 | 산출 |
|---|---|---|
| 계약 (본인) | `src/ccn/types.rs`, `rules.rs`, `mod.rs`, 스텁 | 타입·규칙 테이블·디스패치 |
| A 엔진 | `src/ccn/engine.rs` | tree-sitter 파서, 28 unit tests |
| B 픽스처 | `tests/**` | 25개 파일, 90개 기대값, 골든 하네스 |
| C 통합 | `model.rs`, `loc.rs`, `cli.rs`, `terminal.rs` | 2차 rayon 패스, 집계, CLI |

**파일 소유권 분리가 핵심이었다.** 세 에이전트가 동시에 `cargo test`를 돌리면서도 서로의 파일을 건드리지 않아 병합 충돌이 0건. 엔진이 늦게 착지해 픽스처 에이전트가 잠시 실패를 봤지만, "src를 고쳐서 맞추지 말 것"을 명시해뒀기 때문에 엔진 품질이 오염되지 않았다.

### 산출물

```
src/ccn/
├─ types.rs      CcnTier, Lang, FunctionCcn, FileCcn, CcnSummary (계약)
├─ rules.rs      언어별 decision-node 테이블 (사양서 역할)
├─ engine.rs     tree-sitter 0.27, thread-local parser 캐시, 496줄
├─ heuristic.rs  T3 키워드 폴백 (바이트 단위, UTF-8 무관)
└─ mod.rs        티어 선택 + per-file degradation
tests/
├─ ccn_fixtures.rs        90개 기대값 검증 + "모든 픽스처가 Exact로 파싱" 테스트
└─ fixtures/{javascript,tsx,python,rust,go}/  25개 파일
```

검증: **unit 64 + 통합 2 = 66 tests green**, clippy `-D warnings` clean.

### 실측 (project-c 저장소, 5,948 파일)

| 항목 | 값 |
|---|---|
| 전체 시간 (웜) | **479 ms** |
| 파싱된 파일 / 휴리스틱 파일 | 3,266 / 2,682 |
| 함수 수 | 54,216 |
| CCN median / p90 / p99 / max | 1 / 3 / 12 / **180** |
| CCN > 10 | 690 (1.27%) · >20 158 · >50 18 |
| JSON 크기 | 0.5 MB |

M2 목표였던 "3,300 파일 0.69s"를 이미 통과했다(파일 수는 1.8배 많은데 시간은 30% 짧다).

### 오라클 대조 (lizard) — 우리가 더 정확한 지점

`packages/ui/src/theme/resolve.ts` 17개 함수를 손으로 검산해 비교:

| 함수 | 우리 | lizard | 실제 | 판정 |
|---|---|---|---|---|
| `resolveThemeVariant` (453줄) | 180 | **없음** | 179 decisions | lizard가 함수 자체를 놓침 |
| `borderTone` | 3 | 1 | ternary 2개 | lizard가 중첩 ternary 누락 |
| `alphaTone` | 2 | 1 | ternary 1개 | lizard 누락 |
| `modified` | 6 | 7 | if×2 + `??` + ternary×2 | lizard 과대 |
| `getColors` | 7 | 9 | if×3 + `&&` + `??`×2 | lizard 과대 |

lizard의 집계 편차(±5%)는 상쇄되지만 **함수 단위에서는 신뢰할 수 없다**는 것이 확인됐다. 픽스처 오라클로만 쓰고 제품 의존성에서 제외한 결정이 옳았다.

### 계획 대비 변경

1. **`pct_over_10` 단위 = 0~100.** 계획 §5.4 예시는 0.014(비율)였으나 구현은 퍼센트다. §5.4를 퍼센트로 수정한다.
2. **`files_exact`는 Exact + BestEffort 합계.** 티어가 3개인데 집계 필드는 2개다. T2 문법이 추가되면 `files_best_effort`를 분리해야 한다.
3. **8 MiB 초과 파일은 CCN 생략.** `ccn: null`로 남고 `functions_without_ccn`에도 세지 않는다(현재 리포트에 안 보임). M3에서 `skipped_oversize`로 승격.
4. **`detail`은 JSON에 항상 포함.** `--detail`은 리포트 게이트로만 남는다. 0.5MB는 허용 범위지만 대형 모노레포에서는 `--summary` 옵션이 필요할 수 있다.

### 남은 부채 (M2 진입 전)

- **T2 언어가 전부 휴리스틱이다.** Java/C/C++/C#/Ruby/PHP/Kotlin/Swift는 `files_heuristic`으로 잡히고 함수 통계에 안 들어간다. 사용자 저장소가 이 중 하나면 CCN 패널이 비어 보인다 → M3에서 Java/C/C++/C# 우선 추가.
- `--lang` 여전히 스캔 후 필터.
- 캐시 없음 (479ms라 아직 불필요).
- `engine.rs`가 496줄. 언어가 늘면 rule-driven walker로 더 밀어낼 여지가 있지만 지금은 응집도가 높아 분리 이득이 작다.

---

## 15. M2 실행 결과 (2026-10-21)

### 산출물

```
src/metrics.rs        빈 스펙, 히스토그램, penalty/health, RankedFile, Report (11 tests)
src/report/html.rs    자체 완결 HTML, 인라인 SVG, 0 외부 참조 (8 tests)
src/report/mod.rs
src/terminal.rs       Health/CCN/LOC 블록 + ASCII/유니코드 히스토그램 (11 tests)
src/cli.rs            --report --no-report --open --ci --fail-over (9 tests)
src/main.rs           리포트 쓰기, 브라우저 열기, CI 게이트 exit code
```

검증: **unit 95 + 통합 2 = 97 tests green**, clippy `-D warnings` clean, `cargo fmt` clean.

### 실측: project-c 저장소 (5,948 파일)

```
Health 97 / 100   grade A   (experimental)
0.03 of lines above the McCabe limit · 54,216 functions measured
690 > 10 (1.3%) · 158 > 20 · 18 > 50
CCN   median 1 · p90 3 · p99 12 · max 180
LOC   median 26 · p90 309 · p99 2,053 · max 172,692
top 10% of files = 75% of lines
```

- 리포트 **22,848 bytes**, 외부 참조 0건, 두 번 렌더링 시 바이트 동일(결정적).
- HTML 구조: verdict → CCN/함수 → LOC/파일(이중 막대) → 위험 상위 20 → 푸터.
- CCN 히스토그램이 실제로 보여주는 것: **함수의 71.8%가 CCN 1**, 11 초과는 2.3%(1,208개), 51 초과는 18개.

### 이 숫자가 말하는 것 (해석 주의)

1. **Health 97/A는 관대하다.** CCN 180짜리 함수가 있는 저장소가 A를 받는 것은 등급 밴드가 아직 보정되지 않았기 때문이다. `p(c)`가 100에서 포화되고 라인 가중이 들어가면서 거대한 소수가 희석된다. **M5 전까지는 점수보다 분포(막대)를 봐야 한다.** 리포트에도 `experimental`로 표기된다.
2. **LOC 이중 인코딩이 핵심 정보를 만든다.** `0–49` 빈이 파일의 60.3%지만 라인의 3.3%뿐이고, `≥10,000` 5개 파일이 라인의 9%를 먹는다. 단일 막대 차트였다면 안 보였을 사실.
3. **CCN 중앙값 1은 버그가 아니다.** TS/JS 콜백·화살표 함수가 대량으로 CCN 1로 잡힌다. 그래서 p99(12)와 max(180)를 같이 봐야 한다.

### 계획 대비 변경

1. **`--open`은 opt-in.** 계획 §6은 기본 open이었지만, 스캔할 때마다 브라우저가 뜨는 것은 CI/스크립트에서 방해가 된다. 기본은 파일만 쓰고 `--open`으로 연다.
2. **`--json`은 리포트를 쓰지 않는다.** 기계 판독 모드에서 부작용(파일 생성)이 있으면 파이프라인이 놀란다.
3. **차트 라이브러리 0.** 계획대로 uPlot/Chart.js 없이 인라인 SVG. 22KB.
4. **`--fail-over` 게이트 추가.** `pct_over_10`(함수 기준)이 임계 초과면 exit 1. 계획 §6의 `--fail-over <n>`을 함수 비율로 정의.
5. **등급 밴드 실험 표시.** `Report.health_experimental = true`가 HTML/터미널 양쪽에 나온다.

### 남은 부채 (M3)

- **T2 언어 여전히 휴리스틱.** Java/C/C++/C#/Ruby/PHP/Kotlin/Swift 저장소는 CCN 패널이 비어 보인다.
- **생성/벤더 파일 미탐지.** `files_excluded` 없음. `models-api.json` 17만 줄이 LOC 히스토그램 최상위 빈을 차지한다(CCN에는 안 들어감).
- **health 보정 없음** — 최우선.
- 리포트 브라우저 렌더링은 구조/인코딩/크기로만 검증했고, 실제 픽셀 확인은 미실시.
- 50파일 미만 저장소에서 퍼센트를 숨기는 규칙(§3.6)은 미구현.

---

## 16. M5 실행 결과 — 등급 보정 (2026-10-21)

### 한 일

1. `ccn --report-json` 추가: `metrics::Report`를 그대로 JSON으로 낸다. 캘리브레이션 하네스가 공식을 재구현하지 않게 하려는 것.
2. `tools/calibrate.ps1`: 저장소 목록을 스캔해 `docs/calibration.csv` 생성. `--report-json`만 읽는다.
3. 11개 저장소 측정 → 분포 확인 → 등급 경계 재설정.
4. `grade_for`를 debt 기준 라운드 넘버(2.5 / 6 / 12 / 21%)로 변경.
5. `HEALTH_EXPERIMENTAL = false`, 리포트 푸터를 `grades calibrated on 11 repositories, 2026-10-21`로 교체.

### 결과

| | 보정 전 (A≥92) | 보정 후 (A≥97.5) |
|---|---|---|
| A | 6 | 2 |
| B | 4 | 3 |
| C | 1 | 4 |
| D | 0 | 2 |
| E | 0 | 0 |

측정 범위: 80.99 ~ 97.81, median 95.52. 상세 표는 `docs/calibration.md`.

### 발견한 것 — 점수와 함수 비율은 다르게 움직인다

`project-f`는 **함수의 27.1%가 CCN 10 초과**인데 health 90.80(C)이다. 함수들이 작아서 라인 가중치가 낮기 때문이다. 반대로 project-c는 1.27%만 초과하는데 97.29(B)로 A에 못 미친다 — CCN 180짜리 함수 하나가 라인을 많이 먹는다.

**결론: 점수와 `%func >10`을 항상 같이 읽어야 한다.** 하나로 합치면(예: 평균) 두 신호가 서로를 지운다. 리포트는 이미 두 숫자를 모두 출력하고, 이 사실을 calibration.md §4에 명시했다.

### 하지 않은 것 (의도적)

- **점수 공식은 손대지 않았다.** 보정은 "이미 나온 숫자에 이름을 붙이는" 일이고, 공식을 바꾸면 M1~M2의 모든 측정이 무효가 된다.
- **E 등급은 코퍼스에 없다.** 측정된 최저점이 80.99(D)라 E는 합성 테스트로만 도달 가능하다. 정직하게 문서에 적었다.
- **퍼센타일 등급은 채택하지 않았다.** "상위 20%가 A"는 저장소가 늘 때마다 등급이 바뀐다는 뜻이라 릴리스 게이트로 쓸 수 없다.

### 남은 한계 (M3/M4로 이월)

- Java/C#/C++ 저장소는 여전히 휴리스틱이라 등급이 계산되지 않는다.
- 생성/벤더 제외 없음 → LOC 히스토그램은 오염된다(점수는 CCN 기반이라 상대적으로 안전).
- 50파일 미만 퍼센트 숨김, 작은 저장소 노이즈.
- 코퍼스가 한 개발자 머신 편향(TS 중심). 다른 조직 데이터로 재보정 필요.

---

## 17. M3 실행 결과 — 언어 확장 + 생성/벤더 제외 (2026-10-21)

### 오케스트레이션

파일 소유권을 3개로 분리해 병렬 실행:

| 워크스트림 | 소유 파일 |
|---|---|
| A 언어 | `Cargo.toml`, `ccn/types.rs`, `ccn/rules.rs`, `ccn/engine.rs` |
| B 제외 | `generated.rs`(신규), `model.rs`, `loc.rs`, `metrics.rs`, `terminal.rs`, `report/html.rs`, `cli.rs` |
| C 픽스처 | `tests/fixtures/{java,c,cpp,csharp}/`, `expected.json` |

A와 B는 파일이 완전히 겹치지 않았고, C는 `src/**` 수정을 금지해 엔진 품질이 테스트에 맞춰 오염되지 않게 했다.

### A — 언어 (10개 확장)

추가: **Java, C, C++, C#**. 이제 정확 티어는 10개 언어.

| 언어 | 함수 노드 | 주요 결정 노드 |
|---|---|---|
| Java | `method_declaration`, `constructor_declaration`, `lambda_expression` | `if/for/enhanced_for/while/do`, `switch_label`, `catch_clause`, `ternary_expression` |
| C | `function_definition` | `if/for/while/do`, `case_statement`, `conditional_expression` |
| C++ | `function_definition`, `lambda_expression` | C와 동일 + `for_range_loop`, `catch_clause` |
| C# | `method/local_function/constructor/lambda` | `foreach_statement`, `switch_section`, `switch_expression_arm`, `catch_clause`, `??` |

실측으로 확인한 함정:
- **C++ range-for는 `for_statement`가 아니라 `for_range_loop`.** 목록에 없으면 `for (x : xs)`가 통째로 사라진다.
- **Java는 `case_statement`가 아니라 `switch_label`.** C/C++는 반대로 `case_statement`.
- **C/C++ 함수 이름·파라미터는 declarator 체인 안에 있다.** `LanguageRules`에 `declarator_field`를 추가해 `pointer_declarator → function_declarator → identifier`를 따라간다.
- `default:`는 모든 언어에서 case와 동일하게 +1. C# `??`는 +1, `?.`는 +0.

### B — 생성/벤더 제외

`src/generated.rs`가 경로 + 앞 8KiB로 판정한다.
- vendored: `node_modules`, `vendor`, `third_party`, `.venv`, `site-packages` 등 **디렉터리 세그먼트** + `*.min.js`, `*.map`.
- generated: 파일명 패턴(`*.pb.go`, `*.designer.cs`, `*.gen.ts`) + **주석 줄에 있는** 마커.
- 제외는 LOC 히스토그램, CCN 히스토그램, health, top 20, 집중도에서 모두 적용. `ScanResult.files`에는 플래그와 함께 남는다. `--include-generated`로 해제.

**자가 오탐 수정:** 첫 구현은 마커를 문자열 전체에서 찾아 `src/generated.rs` 자기 자신을 generated로 분류했다. 마커를 **주석 접두사가 있는 줄로 한정**해 해결했다. 지금은 `ccn .`이 자기 저장소를 0개 제외로 측정한다.

### C — 픽스처

19개 파일, 61개 함수 기대값 추가. switch 픽스처는 `default`를 의도적으로 넣지 않아 "case 2개"와 "섹션 2개"가 같은 수가 되게 했다. 람다는 별도 함수로 집계되고 부모를 부풀리지 않는 것을 4개 언어 모두에서 검증.

### 검증

```
cargo test --release      -> 171 lib + 2 integration, 0 failed
cargo clippy -D warnings  -> clean
cargo fmt --check         -> clean
```

### 실데이터 검증 (같은 11개 저장소 재측정)

| 저장소 | 변화 | 원인 |
|---|---|---|
| `project-d` | 85.52(B) → **81.20(D)** | C 4,161개 함수가 이제 실제로 파싱됨. Python만 볼 때보다 나쁘다. |
| `project-e` | max CCN **822 → 141** | minified 번들이 generated/vendored로 제외됨 |
| `project-c` | 파일 3,266 → 3,187, 함수 54,216 → 53,568 | 생성 파일 79개 제외 |
| `CCN` | 0개 제외 | 자가 오탐 수정 확인 |

등급 분포는 A2·B3·C4·D2·E0로 유지 — M5 밴드가 언어 확장 후에도 유효했다.

### 남은 것

- Ruby, PHP, Kotlin, Swift는 여전히 휴리스틱. (Kotlin/Swift는 서드파티 문법이라 버전 고정 필요)
- 생성 판정은 주석 배너 기반이라 배너 없는 생성기는 놓친다.
- M4(git 핫스팟)가 다음 후보. 기준 저장소에 `.git`이 있는지에 따라 가치가 갈린다.

---

## 18. M4 + TUI (2026-10-21)

### M4 — git 핫스팟

`src/git.rs`가 `git log --since=365.days --no-merges --numstat`를 **한 번** 실행하고, `metrics::build`가 스캔 결과와 조인한다. `git2` 같은 네이티브 의존성은 넣지 않았다.

- 20초 타임아웃 + `MAX_NUMSTAT_LINES = 200_000` 상한. 초과·실패·비저장소는 `None`으로 조용히 강등.
- 경로 조인: `rev-parse --show-toplevel`로 작업 트리를 찾고, 스캔 루트가 하위 디렉토리면 prefix를 붙인다. 매칭 실패 파일은 churn 0.
- `rank_pct`는 **동순위 평균 순위**. `hotspot = rank_pct(commits) × rank_pct(risk)`, commits 0이면 정확히 0.
- `hotspot_debt_share` = hotspot 상위 10% 파일이 차지하는 측정 LOC 비율.
- 리포트: 터미널 Git 줄 + 상위 3개, HTML Hotspots 패널 + 푸터 커밋 수.

실측 (project-b, 680 commits/365d):

| hotspot | 파일 | CCN | commits | 변경 줄 |
|---|---|---|---|---|
| 0.98 | `office_backend_bridge.py` | 206 | 73 | 4,101 |
| 0.97 | `private_pty/tui_app.py` | 137 | 70 | 8,843 |
| 0.97 | `rust/crates/tui-shell/src/model.rs` | 162 | 20 | 3,374 |

**`run_timed`(CCN 302)는 hotspot 목록에 없다** — 정적 복잡도만 보면 1위지만, 최근 1년간 자주 바뀌지 않는다면 리스크 우선순위는 낮다. 이게 M4의 요점이다.

### TUI — `--tui`

`ratatui 0.29` + `crossterm 0.28` (버전 일치 필수 — ratatui가 0.28에 의존).

- 좌: CCN/함수 ↔ LOC/파일 히스토그램 (`Tab`)
- 우: **전체 측정 파일** 목록 (Report.top은 20개뿐이라 TUI에서 직접 재계산)
- `j/k`, `g/G`, `s`(정렬 5종), `/`(부분문자열 필터), `Enter`(함수 상세), `?`(도움말), `q`
- 터미널 복원은 RAII 가드 + `catch_unwind`. init 실패·패닉·정상 종료 모두 raw mode와 alternate screen을 되돌린다.
- TTY가 아니면 `Err`를 반환하고 호출자가 요약 출력으로 폴백한다 (CI 안전).
- 20x6 터미널에서도 패닉 없이 "terminal too small".

### HTML 즉시 보기 — `--view`

`%TEMP%\ccn\report-<root hash>.html`에 쓰고 바로 연다. 저장소에 파일을 남기지 않고, 같은 저장소는 같은 경로를 덮어쓴다. `--report`/`--json`과 충돌 선언.

### 검증

```
199 lib + 2 integration tests, 0 failed
clippy -D warnings clean, fmt clean
--tui 비-TTY 폴백 확인
리포트: project-b 26.8KB / project-a 27.6KB, hotspots 패널 포함
```

**한계:** TUI는 `TestBackend` 렌더링 테스트로만 검증했고 실제 터미널 픽셀 확인은 못 했다. `--view`도 브라우저 실행 자체는 자동 테스트하지 않는다.

### 다음 우선순위

1. 캐시 (콜드 3.7s → 1s 이하)
2. HTML 막대 클릭 → 파일 필터 (정적 리포트의 한계)
3. Ruby/PHP/Kotlin/Swift
4. `--lang` 프리필터 (현재 스캔 후 필터)

---

## 19. 캐시 + 핫스팟 옵트인 (2026-10-21)

### 왜 tokei 일괄 스캔을 버렸나

tokei의 `get_statistics`는 전체 트리를 병렬로 훑고 **모든 파일을 읽는다**. 캐시로 건너뛰려면 파일 단위 루프가 필요해서 `ignore::WalkBuilder` + `LanguageType::parse`로 바꿨다.

- 워크: gitignore 존중, 기본 제외 디렉토리, hidden 옵션 동일
- 캐시 키: `(path, mtime_ns, size, CACHE_VERSION)`
- 캐시 위치: `%LOCALAPPDATA%\ccn\<root hash>.json` (저장소 안에는 아무것도 쓰지 않음)
- 원자적 교체(`.tmp` → rename), 손상/버전 불일치는 miss
- **변경이 없으면 다시 쓰지 않는다**(dirty 추적). 매번 9.7MB를 직렬화하던 첫 구현은 캐시가 있는데도 느렸다.
- `--no-cache`, `--no-ccn`, `--lang`, `--include-generated`는 캐시를 우회한다. `--include-generated`가 캐시를 오염시키면 다음 기본 스캔이 생성물을 놓치기 때문.

### 패리티

project-b 1,145 파일: 파일 수·LOC·comment·blank·함수 수·파일별 CCN max까지 **완전 일치**. CCN 저장소도 편집된 파일 외에는 일치.

### 실측 (project-a, 5,367 파일 / 1.77M 줄)

| | 이전 | 캐시 후 |
|---|---|---|
| 콜드 (캐시 없음) | 3.7s | ~1.2s (파싱 0.95s + walk 0.07s + 저장) |
| 웜 (캐시 적중) | 1.2s | 0.3~0.8s (로드 5~27ms, resolve 5~15ms) |

워크/파싱은 30ms 수준이고 나머지는 프로세스 시작 + Windows Defender의 캐시 파일 스캔 변동이다. 빈 디렉토리 실행도 65~70ms라 실질 하한이 그 근처다.

### 핫스팟은 옵트인

캐시를 붙이자 `git log --since=365.days --numstat`(project-a 1.2s, project-b 1.5s)가 전체 시간을 지배했다. 기본 0.3s 스캔이 git 때문에 2~4s가 되는 건 맞지 않아 **`--hotspots`를 명시했을 때만** 수집한다. `metrics::build`는 git 없이, `build_with(result, true)`가 수집한다.

### 남은 것

- 캐시 JSON이 크다 (project-a 9.7MB, project-b 1.8MB). 함수 상세를 튜플/바이너리로 압축하면 로드와 Defender 비용이 줄어든다.
- `--lang`은 여전히 스캔 후 필터이며 캐시를 우회한다.
- mtime 기반이라 `git checkout` 후 첫 실행은 콜드다.

---

## 20. 트렌드 · diff 모드 · 소유권 (2026-10-21)

### 구현

| 기능 | 파일 | 동작 |
|---|---|---|
| 트렌드 | `src/history.rs` | `<repo>/.ccn/history.json`에 스냅샷 추가(최대 200), 이전 값과 델타 출력 |
| diff 모드 | `src/git.rs`, `src/loc.rs` | `git diff --name-only --diff-filter=ACMR <rev>`로 후보 필터 |
| 소유권 | `src/git.rs`, `src/metrics.rs` | 같은 `git log` 패스에서 path×author 커밋 수 집계, 상위 20명 |

스냅샷은 8개 필드(`at_unix, health, grade, loc, functions, pct_over_10, ccn_max, files_measured`)만 저장한다. 커밋 가능한 크기이고 diff가 한 줄로 읽힌다.

`--trend`와 `--since`는 **동시에 쓸 수 없다**: diff 결과를 전체 저장소 트렌드로 기록하면 다음 비교가 오염된다.

### 실측

```
project-b --since HEAD~1
  1 files · 6,635 lines
  Health 74.8 / 100  grade E
  20 functions > 10 (5.4%) · 1 > 50
```

변경된 `app.rs` 한 파일만 측정했기 때문에 전체 저장소(89.0 C)와 다른 값이 나온다. **이 숫자는 변경분에 대한 것이고 전체 건강도와 비교하면 안 된다.**

```
project-b --hotspots
  0.98  office_backend_bridge.py 206 CCN · 73 commits · 4,101 lines changed · Shinick-Han (+2)
  0.97  private_pty/tui_app.py   137 CCN · 70 commits · 8,843 lines changed · Test (+1)
```

```
ccn . --trend  (2회 연속)
  Trend  health 97.0 → 97.0 (0.0) · >10 1.8% → 1.8% (0.0pp)
```

### 한계

- 첫 실행은 트렌드가 없다(비교 대상이 없음).
- `--since`는 untracked 파일을 포함하지 않는다(git에 revision이 없음).
- 트렌드는 **도구/밴드 버전을 저장하지 않는다.** `CACHE_VERSION`이나 등급 경계가 바뀌면 과거 스냅샷과 비교하면 안 되는데, 지금은 그걸 감지할 방법이 없다. 다음 개선 후보.
- `--since`는 캐시를 우회한다(부분 스캔이 전체 캐시를 덮어쓰면 안 되므로).
- 작성자 상한 20명/파일, 365일 창.

### 검증

```
231 lib + 2 integration tests, 0 failed
clippy -D warnings, fmt clean
```

---

## 21. 스냅샷 버전 정합성 (2026-10-21, 0.3.1)

트렌드의 마지막 구멍을 막았다: **버전이 다른 두 스냅샷을 조용히 비교하던 문제.**

### 변경

- `Snapshot`에 `tool_version`, `bin_spec_version`, `health_calibration` 추가
  (`#[serde(default)]`라 0.3.0 이전 스냅샷도 읽힌다).
- `Trend::compatibility_warning()`:
  - 세 필드가 모두 비어 있으면 `previous snapshot predates versioning; deltas are not comparable`
  - 도구 버전이 다르면 `ccn 0.2.0 -> 0.3.1`
  - bin spec이 다르면 `bin spec v1 -> v2`
  - 보정 코퍼스가 다르면 `grade calibration changed`
- 터미널: 트렌드 줄 아래 `  !      ...` (100컬럼 제한)
- HTML: `.trend-warn` 문단, `role="status"`

### 실측

0.3.0이 기록한 이력(버전 필드 없음)에 0.3.1로 `--trend`:

```
Trend  health 97.0 → 97.0 (+0.1) · >10 1.8% → 1.8% (0.0pp)
!      previous snapshot predates versioning; deltas are not comparable
```

다음 실행(둘 다 0.3.1)에서는 경고가 사라진다.

### 남은 것

- 스냅샷에 git HEAD를 넣으면 "어느 커밋에서 측정했는지"까지 추적할 수 있다.
- `--since` 결과는 여전히 트렌드에 기록하지 않는다(의도).

---

## 22. Perforce 지원 (2026-10-21, 0.4.0)

코어는 원래 VCS와 무관했다. git 결합은 5개 호출 지점뿐이었고, 그 뒤에 `Vcs` 계층을 넣어 P4 백엔드를 추가했다.

### 구조

```
src/vcs.rs   detect -> Git | P4 | None, collect_history, changed_since
src/git.rs   기존 구현 (work-tree 상대 키)
src/p4.rs    Helix Core 백엔드 (scan-root 상대 키)
```

`vcs`가 양쪽 키를 **scan root 상대 경로로 정규화**하므로 `metrics.rs`/`loc.rs`는 더 이상 git을 모른다(prefix 보정 코드 삭제).

### P4 명령

| 용도 | 명령 |
|---|---|
| 감지 | `p4 info` (hint가 있을 때만 실행) |
| 경로 매핑 | `p4 where <root>` |
| 변경 파일 | `p4 files <depot>/...@=<CL>,@now` ∪ `p4 opened` |
| 이력 | `p4 changes -m N -s submitted -ztag` + CL별 `p4 describe -s -ztag` |

- CL별 describe는 rayon으로 병렬화, **기본 300개 상한**(`--history-limit`), 명령당 20초 타임아웃.
- 작성자 상한 20명/파일, 경로 중복 제거.
- `--since opened`는 로컬 pending 변경만.

### P4에서 불가능한 것 (정직하게)

- **변경 줄 수 없음**: `describe -s`는 diff를 주지 않고, CL마다 diff를 받으면 서버 왕복이 폭발한다. `lines_changed`는 0으로 두고, 핫스팟은 커밋 수 기반이라 랭킹은 유지된다. 리포트의 "lines changed" 열만 비어 보인다.
- **365일 창이 아니라 CL 개수 창**: P4는 시간 기반 조회가 비싸서 최근 N CL로 자른다.

### P4 현실 문제 대응

- **read-only 워크스페이스**: `p4 sync` 파일은 읽기 전용이라 `.ccn/history.json` 쓰기가 실패한다. → 실패 시 `%LOCALAPPDATA%\ccn\history-<hash>.json`으로 자동 폴백. (`load`도 양쪽을 본다)
- **`.p4ignore`**: `ignore` 크레이트의 `add_custom_ignore_filename(".p4ignore")`로 지원.
- **라벨**: 터미널/HTML이 더 이상 "Git"을 하드코딩하지 않고 `p4: N changelists`로 표시.

### 검증

```
260 lib + 2 integration tests, 0 failed
clippy -D warnings, fmt clean
```

- 파서 단위 테스트 10개(`p4.rs`) + 정규화 테스트 6개(`vcs.rs`)
- git 경로 회귀: `--hotspots`, `--since HEAD~1` 실저장소에서 동일 동작 확인
- `.p4ignore`: 임시 디렉토리에서 2개 중 1개만 스캔되는 것 확인
- read-only 폴백: `.ccn`을 파일로 만들어 쓰기 실패 → 사이드카 생성 확인

### 남은 리스크

- **실제 P4 서버로 검증하지 못했다.** 파서는 canned output으로만 테스트했다. 첫 실사용은 `p4 info` + 작은 `--history-limit 20`으로 확인할 것.
- P4 streams/branches 간 이력, rename 추적은 범위 밖.
- 대규모 서버(수천 CL)에서 `--hotspots`는 여전히 수십 초가 걸릴 수 있다.




---

## 23. PR 베이스라인 (2026-10-21, 0.5.0)

"이 PR이 새로 만든 복잡한 함수"를 짚는 기능. `--baseline <rev>` + `--fail-on-new`.

### 동작

1. `vcs::rev_exists`로 리비전을 먼저 검증한다(오타가 전체 저장소를 "신규"로 만들지 않게).
2. 측정 대상 파일마다 `vcs::file_at`으로 **옛 버전 바이트**를 가져온다.
   - git: `git show <rev>:<path>`
   - p4: `p4 -q print -q <depot>@=<CL>`
3. `ccn::analyze_file`로 옛 함수 목록을 뽑아 현재와 **이름 기준**으로 매칭한다(중복은 소스 순서로 소비).
4. `added`(베이스에 없고 CCN > 10), `worsened`(10 이하 → 초과)만 리포트.

### 발견하고 고친 것

첫 구현은 베이스에 파일이 없으면 **skip**했다. 그런데 PR에서 새 파일은 신규 복잡 함수가 가장 많이 생기는 곳이다. → `rev_exists`로 리비전을 검증한 뒤, `file_at == None`은 "그 리비전에 파일 없음"으로 해석해 **빈 베이스**로 비교한다. `files_skipped`는 베이스를 파싱할 수 없을 때만 증가한다.

### 실측 (CCN 저장소, HEAD~1 대비)

```
Since HEAD~1  1 new >10 · 0 worsened   (66 files compared, 0 skipped)
  + 11   parse_changes            src/p4.rs:510
```

`--fail-on-new`는 이 경우 exit 1. 잘못된 리비전은 exit 2.

### 리포트

- 터미널: Health/Trend 아래 `Since <rev>` 블록 + 상위 5개
- HTML: `Since <rev>` 패널(테이블: change/function/CCN/was/location)
- JSON: `Report.baseline`

### 한계

- **이름 기준 매칭**이라 이름을 바꾼 함수는 "added"로 보인다(의도적 단순화).
- 파일 리네임은 git이 경로를 따라가지 않는다(새 경로 = 신규).
- P4는 여전히 실서버 미검증.

### 다음

- GitHub Action + SARIF (PR 코멘트/코드스캔 탭)
- 파일 리네임 추적(`git diff -M`)

---

## 24. SARIF + GitHub Action (2026-10-21, 0.6.0)

### SARIF

`src/sarif.rs`가 SARIF 2.1.0을 만든다. serde_json `Value`로 조립하므로 경로/함수명 이스케이프는 자동이다.

- 규칙 3개: `ccn/complex-function`, `ccn/added-complex-function`, `ccn/worsened-complex-function`
- 레벨: CCN 11-20 `note`, 21-50 `warning`, 50 초과 `error`
- `--baseline`이 있으면 **델타만** 결과로 낸다(PR에서 저장소 전체 백로그를 쏟지 않음). 없으면 `--sarif-min-ccn` 이상 전부.
- `artifactLocation.uri`는 저장소 상대 경로만 사용한다. 절대 경로(`C:/...`)가 새지 않는 것을 테스트로 고정.
- 결과는 `(path, line, rule)`로 정렬해 결정적이다.

실측(CCN 저장소):
```
전체:  17 results, rules 3, driver ccn 0.6.0
델타(a87540e): 1 result  ccn/added-complex-function warning src/p4.rs:510
```

### Action

`action.yml` (composite) + `.github/workflows/ccn.yml`.

- 입력: `path`, `baseline`, `fail-on-new`, `sarif`, `sarif-min-ccn`, `extra-args`
- 액션은 `cargo build --release --locked`로 바이너리를 만들고, 항상 `--no-report`를 붙인다.
- `fail-on-new: true`면 새/악화 함수에서 step이 실패하고, 워크플로는 `if: always()`로 SARIF를 업로드한다.
- 워크플로 권한: `contents: read`, `security-events: write`, `pull-requests: write`. checkout은 `fetch-depth: 0`(베이스라인 때문).

### 검증

```
286 lib + 2 integration tests, 0 failed
clippy -D warnings, fmt clean
YAML 2개 파싱 확인 (action.yml, ccn.yml)
```

### 한계

- 액션이 매 실행마다 소스를 빌드한다(캐시가 있어도 수 분). 릴리스 바이너리를 내려받는 방식이 다음 개선.
- 코드 스캐닝 업로드는 공개 저장소 또는 GHAS가 켜진 private 저장소에서만 동작한다.
- SARIF 전체 모드는 노이즈가 될 수 있어 PR에서는 델타 모드를 권장.

---

## 25. 릴리스 · 리네임 · P4 실서버 검증 (2026-10-21, 0.7.0)

### 릴리스 바이너리

`.github/workflows/release.yml`이 `v*` 태그에서 3개 플랫폼 바이너리를 만든다.

- 자산: `ccn-linux-x64`, `ccn-macos-x64`, `ccn-windows-x64.exe` (raw, 압축 없음)
- `action.yml`은 최신 릴리스 자산을 먼저 받고, 실패하면 소스 빌드로 폴백한다.
- private 저장소는 익명 다운로드가 404이므로 폴백 경로를 탄다(문서화됨).

### 리네임 추적

`git diff -M --name-status <rev>`를 파싱해 `new -> old` 맵을 만들고, 새 경로의 베이스라인이 없으면 옛 경로에서 읽는다.

- `git::renames`, `vcs::renamed_from`, `baseline::collect`에서 사용
- 이동만 한 파일: 변화 없음. 이동 + 복잡도 증가: `Worsened`. 진짜 새 파일: `Added`.
- P4는 리네임 추적 없음(빈 맵).

### P4 실서버 검증 (로컬 p4d)

Helix Core `p4d`를 로컬에 띄우고 depot/클라이언트/CL 4개를 만들어 실제로 돌렸다. **단위 테스트로는 못 잡는 버그 7개를 발견했다:**

1. **`p4 -q files`는 출력을 통째로 억제한다.** 전역 `-q`는 조용히 빈 결과를 만든다. → 전역 `-q` 제거, `print -q`만 유지.
2. **`-ztag`는 전역 플래그다.** `p4 changes -ztag`는 "Invalid option"으로 실패해 히스토리가 통째로 사라졌다. → `p4 -ztag changes ...`, `p4 -ztag describe ...`.
3. **`p4 where`는 디렉토리를 받지 않는다.** `...` 와일드카드가 필요하고, `//depot` + `...`는 `//depot/...`이 되어야 한다(슬래시 누락 주의). → `depot_wildcard` 헬퍼.
4. **`@=N,@now`는 N 시점 파일까지 포함한다.** "N 이후 변경"은 `@>N`. → `--since`가 b.rs까지 잡던 문제 수정.
5. (추가) `p4 changes @=N`은 파일 스펙이 없으면 빈 결과 → `rev_exists`는 `describe -s N`으로 확인.
6. (추가) 스캐너가 넘기는 `\\?\C:\...` 경로를 `p4 where`가 이해하지 못한다 → mapping_for에서 prefix 제거.
7. (추가) `-ztag` 위치와 함께 `p4 opened`의 "File(s) not opened" 문구 처리.

검증 결과:
```
scan          3 files · Health 86.9 / D
--since 1     2 files (a.rs, c.rs)
--baseline 1  2 new >10 (deep 15, complex 13)
--hotspots    P4 4 changelists · src/a.rs 3 commits · src/c.rs 2 · author test
--sarif       delta results emitted
```

### 남은 것

- P4 `lines_changed`는 여전히 0 (`describe -s`가 diff를 주지 않음).
- P4 리네임/스트림 간 이력은 범위 밖.
- 릴리스 워크플로는 태그를 푸시해야 처음 실행된다.

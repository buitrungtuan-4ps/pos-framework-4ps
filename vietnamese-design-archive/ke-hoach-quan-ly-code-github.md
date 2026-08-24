# Kế hoạch quản lý code trên GitHub — v2

> **Phân loại:** T3 – Nội bộ. Đi cùng Kiến trúc v2.5 và Đặc tả nghiệp vụ POS v1.
> **Mục tiêu:** biến các nguyên tắc kiến trúc (framework, core độc lập, không phụ thuộc) thành thứ **được máy bắt buộc thi hành**, chứ không phải thỏa thuận miệng.
> **v2 bổ sung:** chuẩn cho framework nhiều người đóng góp — người thật lẫn AI (mục 11–16) và trải nghiệm fork-and-deploy (mục 17). Nguyên tắc trung tâm: **mọi luật khả thi phải do máy thi hành** (lint/CI), vì AI và contributor vãng lai không hấp thụ tri thức ngầm qua review.

---

## 1. Chiến lược repo: monorepo

Một repo **private** duy nhất: `pos-framework` (Cargo workspace như kiến trúc mục 15.2).

Vì sao monorepo, không tách repo mỗi crate:

- **Cloud và edge nói chuyện với nhau bằng một giao thức chung.** Tách repo là mở đường cho hai bên lệch version âm thầm — loại lỗi đắt nhất trong hệ phân tán. Cùng repo thì một PR sửa được cả hai đầu, CI test cả hai cùng lúc.
- Đổi một port kéo theo sửa nhiều adapter → **một commit nguyên tử**, không phải điệu nhảy 5 PR qua 5 repo.
- Một CI, một bộ lint, một `Cargo.lock` → build tái lập được.

**Đảo quyết định ở v2:** manifest triển khai nằm ngay trong monorepo tại `deploy/` thay vì repo phụ `pos-ops` — vì mục tiêu fork-and-deploy (mục 17) đòi hỏi fork MỘT repo là có tất cả. Quyền nhạy cảm bảo vệ bằng GitHub Environments (required reviewer) + CODEOWNERS trên `deploy/`, không cần tách repo.

```
pos-framework/
├── .github/
│   ├── workflows/            ci.yml · release.yml · nightly.yml
│   ├── ISSUE_TEMPLATE/       bug · feature · rfc
│   └── PULL_REQUEST_TEMPLATE.md
├── AGENTS.md                 ← luật cho AI contributor (mục 13)
├── CONTRIBUTING.md           ← cách tham gia, chạy test, checklist PR
├── MAINTAINERS.md · SECURITY.md
├── crates/                   (xem Kiến trúc 15.2)
├── examples/                 ← chạy được thật (mục 14)
├── templates/adapter-template/  ← scaffold adapter mới (cargo-generate)
├── deploy/                   ← compose.yml · Caddyfile · bootstrap.sh · k8s/ (mục 17)
├── ui/
├── docs/
│   ├── architecture.md       ← kiến trúc v2.5
│   ├── pos-spec.md           ← đặc tả nghiệp vụ
│   ├── standards/            ← luật code & tài liệu (mục 11–12)
│   ├── guides/               ← hướng dẫn cho NGƯỜI DÙNG framework
│   ├── adr/                  ← quyết định kiến trúc (mục 8)
│   └── runbook/              ← thay máy, khôi phục, xoay khóa
├── deny.toml                 ← luật giấy phép & bảo mật
├── rustfmt.toml · clippy.toml
├── rust-toolchain.toml       ← ghim phiên bản Rust
└── justfile                  ← lệnh chuẩn: preflight, build, test, sign, release
```

---

## 2. Nhánh & luồng làm việc

**Trunk-based, nhánh sống ngắn.** `main` luôn ở trạng thái phát hành được.

```
  feat/kds-bump ──┐
  fix/print-retry ─┼──► main ──► tag v1.4.0 ──► release/1.4.x
                   │                                │
                   └────────────────────────────────┴─► hotfix: fix trên
                                                        release/1.4.x rồi
                                                        cherry-pick về main
```

- Nhánh tính năng sống **dưới 3 ngày**; dài hơn thì chia nhỏ.
- **Nhánh phát hành `release/1.4.x`** tồn tại vì OTA đi theo vòng: khi Ring 1 đang chạy 1.4.0 mà `main` đã tiến xa, bản vá khẩn phải xuất phát từ đúng cái cây đang chạy ngoài quán — không phải từ `main`.
- Bảo vệ `main` và `release/*`: cấm push thẳng, bắt buộc PR + CI xanh + 1 review, cấm force-push, cấm xóa.

**Quy ước commit (Conventional Commits):** `feat:` `fix:` `perf:` `refactor:` `docs:` `test:` `chore:`, kèm scope là tên crate — `feat(fiscal-vn): ...`. Dùng để tự sinh changelog và tự quyết mức tăng version.

---

## 3. Đánh version — hai trục tách biệt

Đây là điểm dễ sai nhất và phải làm đúng từ đầu.

| Trục | Ví dụ | Ý nghĩa |
|---|---|---|
| **Version sản phẩm** (SemVer) | `v1.4.2` | Phiên bản binary phát hành |
| **PROTOCOL_VERSION** | `3` | Ngôn ngữ cloud↔edge nói với nhau, khai báo trong `pos-proto` |

Vì sao phải tách: edge cập nhật **theo vòng và có thể offline nhiều ngày** → tại một thời điểm luôn có nhiều version edge cùng nối vào một cloud. Luật bắt buộc:

1. **Cloud phải hiểu được ít nhất 2 protocol version gần nhất.** CI có test kiểm chứng điều này.
2. Thay đổi giao thức **chỉ được thêm** (thêm field optional, thêm loại sự kiện). Muốn xóa/đổi nghĩa → tăng `PROTOCOL_VERSION`, chạy song song ≥ 2 bản phát hành rồi mới bỏ bản cũ.
3. Cùng luật với migration SQLite (Kiến trúc 14.3): chỉ thêm, không xóa/đổi trong một bản.

---

## 4. CI — 3 tầng pipeline

**PR (mỗi lần push, mục tiêu < 10 phút)**

```
fmt → clippy -D warnings → test unit (core: không DB, không mạng)
    → build matrix: linux-x86_64 + windows-x86_64
    → cargo-deny (giấy phép + advisory + trùng lặp)
    → gitleaks (chặn secret lọt vào repo)
    → kiểm tra LUẬT PHỤ THUỘC: pos-core/pos-ports chỉ chứa dependency trong allowlist
    → linter ĐẶT TÊN: OpenAPI + SQL migration + registry sự kiện/quyền (Chuẩn đặt tên v1 mục 13)
    → build UI
```

**Main (sau khi merge)**

```
+ integration test (Postgres + NATS thật trong service container)
+ contract test: MỌI implementation của mỗi port phải qua cùng một bộ test
+ simulator smoke: N quán ảo, bơm đơn, giả offline/online, đối soát khớp
```

**Nightly**

```
+ chạy simulator dài (soak) qua đêm
+ diễn tập restore: lấy ngẫu nhiên một backup, restore, kiểm toàn vẹn (Kiến trúc 14.5)
+ cargo-audit theo advisory mới
```

Hai kiểm tra ở tầng PR là thứ **biến kiến trúc thành luật**: kiểm tra luật phụ thuộc giữ cho core mãi mãi thuần (không ai lỡ tay import tokio vào domain), và `cargo-deny` giữ mục tiêu giấy phép.

---

## 5. Kiểm soát giấy phép & độc lập phụ thuộc

**`deny.toml`** — cấu hình theo đúng mục tiêu ở Phụ lục D:

- **Cho phép:** MIT, Apache-2.0, ISC, BSD, Unlicense/public domain.
- **Cảnh báo:** MPL-2.0 (hiện chỉ còn `serialport-rs`, có kế hoạch thay).
- **Chặn cứng:** AGPL, GPL, SSPL trong đồ thị phụ thuộc của binary. (MinIO/Garage/Grafana là dịch vụ chạy riêng, không link vào binary — nhưng ghi chú rõ trong repo là hạng mục sẽ loại bỏ.)
- Chặn crate trùng lặp nhiều version, chặn crate không rõ nguồn.

**Vendoring — điều kiện của "không phụ thuộc" theo nghĩa đen.** Chạy `cargo vendor` và commit thư mục `vendor/` (hoặc lưu bản nén ở nơi mình kiểm soát). Nhờ vậy repo build được **offline vĩnh viễn**, kể cả khi crates.io không truy cập được hoặc một crate bị tác giả xóa (chuyện đã từng xảy ra trong hệ sinh thái npm). Kèm `rust-toolchain.toml` ghim phiên bản Rust để build hôm nay và 3 năm sau cho ra cùng kết quả.

**Cập nhật phụ thuộc:** Renovate/Dependabot gom PR theo tuần, không tự merge — mỗi bản nâng đều phải qua đủ pipeline.

**Repo cũng là một phụ thuộc.** GitHub là nhà cung cấp; mirror `main` + tag sang một remote thứ hai (Gitea self-host hoặc bare clone trên máy văn phòng) bằng một job hằng ngày. Mất tài khoản GitHub thì mất tiện ích, không mất tài sản.

---

## 6. Phát hành & ký — nối thẳng vào OTA rings

```
tag v1.4.0
   │
   ▼
CI build 2 OS + sinh SBOM + tính hash        ← máy CI KHÔNG giữ khóa ký
   │
   ▼
Tải artifact về máy maintainer → ký bằng khóa USB (minisign)   ← thao tác tay, có chủ đích
   │
   ▼
Upload artifact + chữ ký + manifest lên kho phát hành
   │
   ▼
Ring 0 (lab/quán nội bộ) → theo dõi → Ring 1 → Ring 2   (Kiến trúc mục 9)
```

Ký thủ công bằng khóa offline là **có chủ đích**, không phải chưa kịp tự động hóa: nó bảo đảm không tồn tại đường nào để một pipeline bị chiếm quyền tự phát hành phần mềm xuống toàn fleet (Kiến trúc 14.4). Khi nào build đã hoàn toàn ổn định, có thể chuyển sang mô hình CI ký bằng khóa build + maintainer ký duyệt lên fleet — hai chữ ký, hai chủ thể.

**Checklist trước khi tag:** CI xanh · changelog đã sinh · protocol version tương thích ngược ≥ 2 bản · migration chỉ-thêm · đã chạy simulator soak · runbook rollback đọc lại một lượt.

---

## 7. Chiến lược test (theo tầng, khớp cấu trúc framework)

| Tầng | Chạy ở đâu | Tốc độ | Nội dung |
|---|---|---|---|
| Unit — domain thuần | mọi PR | vài giây | Toàn bộ luật nghiệp vụ POS bằng fake in-memory: **không DB, không mạng, không máy in** |
| Contract test cho port | mọi PR | vài chục giây | Mỗi implementation của một port phải qua cùng một bộ test (Kiến trúc 15.5) |
| Integration | main | vài phút | Postgres + NATS thật trong container |
| Fleet simulator | main + nightly | dài | N quán ảo: bơm đơn, ngắt mạng, OTA rings, đối soát đêm |
| Phần cứng | thủ công, theo ma trận | — | Máy in ESC/POS từng hãng, máy cà thẻ, cúp điện đột ngột |

Chỉ tầng cuối cần con người — và đó chính là lý do fleet simulator đáng đầu tư sớm: nó cho phép tra tấn OTA, burst sync và đối soát **trước khi** có quán thật nào chịu trận.

---

## 8. ADR — ghi lại "vì sao", không chỉ "cái gì"

`docs/adr/NNNN-tieu-de.md`, mỗi file: bối cảnh → các phương án đã cân nhắc → quyết định → hệ quả chấp nhận. Đây là thứ giữ cho framework chuyên nghiệp qua thời gian: 6 tháng sau không ai phải tranh luận lại từ đầu, và người mới hiểu được vì sao hệ thống trông như vậy.

**Quyết định phải có trước commit đầu tiên: giấy phép của chính framework** (ảnh hưởng header mỗi file, `LICENSE`, CONTRIBUTING/CLA) — Apache-2.0 (lan rộng, có cấp bằng sáng chế) · AGPL-3.0 (chặn đóng gói lại thành dịch vụ) · BUSL/source-available (cấm thương mại hóa cạnh tranh, tự mở sau N năm) · đóng hoàn toàn (nội bộ). **Đã chốt (ADR-0009): đóng, dùng nội bộ.** `LICENSE` ghi bản quyền độc quyền, header mỗi file, repo private. Các quyết định kỹ thuật giữ nguyên (repo đã sạch AGPL/MPL, `cargo-deny` vẫn chặn copyleft) để mở nguồn về sau vẫn khả thi mà không phải gỡ dependency.

ADR cần viết ngay từ những gì đã chốt: 0001 offline-first tại quán · 0002 một binary mỗi tầng (modular monolith) · 0003 triết lý "máy chết thì thay" + lease · 0004 cấu hình tập trung trên cloud · 0005 core trung lập quốc gia + module tài khóa · 0006 sở hữu ranh giới bằng port · 0007 chiến lược tự viết (Phụ lục D) · 0008 chọn Postgres partition thay vì DB-per-store.

---

## 9. Quyền & bảo mật repo

- Repo private; bật secret scanning + push protection; `.gitignore` chặn `config.toml`, `*.key`, `.env`.
- `CODEOWNERS`: `crates/pos-core/`, `crates/pos-ports/`, `crates/pos-proto/` và `.github/` yêu cầu review của maintainer — đây là các ranh giới không được đổi tùy tiện.
- GitHub Environments cho triển khai cloud, có required reviewer.
- Không bao giờ có khóa ký trong repo hay trong secret của CI (mục 6).

---

## 10. Thứ tự dựng (tuần đầu)

1. Khởi tạo workspace + `rust-toolchain.toml` + `justfile`.
2. Dựng `pos-core` / `pos-ports` / `pos-proto` **trống nhưng đúng ranh giới** — kèm test kiểm tra luật phụ thuộc (làm trước khi có code, để không bao giờ phải sửa ngược).
3. `deny.toml` + pipeline PR.
4. Bảo vệ nhánh + CODEOWNERS + PR template.
5. ADR 0001–0008 (chép lại từ các quyết định đã chốt).
6. Sinh 2 cặp khóa minisign, cất theo Kiến trúc 14.4, ghi runbook xoay khóa.
7. Job mirror repo sang remote thứ hai.

Bảy việc này gần như không viết dòng code sản phẩm nào — nhưng làm sau thì mỗi việc đều phải sửa ngược vào code đã có.


---

# PHẦN II — Chuẩn framework nhiều người đóng góp (người thật + AI)

## 11. Quy tắc code — máy thi hành trước, quy ước sau

**Lint là code, commit vào repo** (`[workspace.lints]` trong Cargo.toml, `rustfmt.toml`, `clippy.toml`) — không phải văn bản để đọc rồi quên. Độ chặt áp theo tầng:

| Tầng | Luật (build fail nếu vi phạm) |
|---|---|
| `pos-core` / `pos-ports` / `pos-proto` | `forbid(unsafe_code)` · cấm `unwrap/expect/panic` (clippy deny) · `deny(missing_docs)` — API public nào cũng phải có rustdoc · cấm `println` (chỉ `tracing`) · dependency ngoài allowlist = fail (đã có từ mục 4) |
| Adapters | `unsafe` được phép cho FFI nhưng **mỗi khối phải có `// SAFETY:`** (lint `undocumented_unsafe_blocks`) · cấm `unwrap` trên đường chạy chính · rustdoc mức warn |
| Binaries & tests | Nới `unwrap` trong test/build script; binary dùng `anyhow` ở tầng ngoài cùng |

**Chính sách lỗi:** crate thư viện trả error enum cụ thể (`thiserror`), không trả `String`; kết quả quan trọng gắn `#[must_use]` — nuốt lỗi là build warning.

**Logging:** `tracing` có cấu trúc, có span; **cấm log PII** (số điện thoại, tên khách phải mask) — đây là luật cứng vì log được kéo về cloud.

**Ổn định API — cơ chế quan trọng nhất của một framework:** ba crate xương sống có **file snapshot public API** trong repo (cargo `public-api` / `semver-checks`). Muốn đổi API phải sửa file snapshot *trong cùng PR* — thay đổi trở nên tường minh, có diff, có review; đổi lén = build fail. Kèm chính sách: deprecate ≥ 2 bản phát hành trước khi xóa; MSRV ghi rõ, chỉ tăng ở bản minor. Snapshot áp dụng cho cả **schema sự kiện public** (webhook/feed) — sự kiện đã công bố là hợp đồng, chỉ được thêm field. Cùng cơ chế cho **danh mục quyền RBAC** (Đặc tả v2 mục 15): thêm quyền = diff snapshot trong PR; xóa quyền = cấm, chỉ deprecate.

**Năm luật bổ sung (vòng rà soát cuối):**
1. **Tiền là số nguyên, cấm float:** lưu bằng đơn vị nhỏ nhất + mã tiền tệ (`i64` + currency); mọi làm tròn qua đúng một hàm tập trung trong `pos-core`. Lint cấm `f32/f64` trong các kiểu tiền.
2. **Core chỉ lấy thời gian & ID qua port** (`ClockSource`, `IdGenerator`) — cấm `SystemTime::now()`/`rand` trực tiếp trong core (CI grep); test nhờ đó điều khiển được thời gian (ca qua nửa đêm, khuyến mãi hết hạn giữa bill).
3. **Cấm blocking trong async:** sync IO / sleep / tính toán nặng trong tokio task phải qua `spawn_blocking` (clippy + review).
4. **Channel và cache trong process phải bounded** — mọi hàng đợi/map trong RAM có giới hạn tường minh; chống rò RAM âm thầm.
5. **Phát sự kiện chỉ qua `TxContext`:** API duy nhất để ghi outbox nằm trong transaction — "quên tính giao dịch" trở thành điều không thể viết ra, không phải điều review phải bắt.
7. **Tuân thủ Chuẩn đặt tên & API v1:** snake_case cho JSON/URL/cột DB/tên sự kiện/mã quyền; thời gian hậu tố `_time`; tiền là `currency_code` + `amount_minor`; enum `UPPER_SNAKE` có `*_UNSPECIFIED`. CI có **linter đặt tên** quét OpenAPI + migration SQL + registry sự kiện/quyền, cộng test đối chiếu tên cột DB ≡ tên trường JSON.
6. **Cấm chuỗi hiển-thị-người-dùng hardcode:** mọi chuỗi UI/biên nhận đi qua khóa dịch ICU (CI grep chuỗi literal trong tầng UI và template in) — thi hành kỷ luật i18n-từ-commit-đầu bằng máy (Đặc tả v2 mục 21).

**Nguyên tắc chấm dứt tranh cãi style:** mọi tranh luận về style trong PR kết thúc bằng một trong hai cách — thêm lint, hoặc bỏ qua vĩnh viễn. Không tranh cãi bằng miệng lần thứ hai.

## 12. Quy tắc tài liệu — 4 tầng, mỗi tầng một luật

| Tầng | Dành cho ai | Luật |
|---|---|---|
| **rustdoc** | Người/AI gọi API | Bắt buộc trên mọi item public của core/ports/proto (CI gác); ví dụ trong doc phải compile (doctest chạy trong CI) |
| **README mỗi crate** | Người mở crate lần đầu | Theo template 4 mục: crate này làm gì · nằm đâu trong kiến trúc · cách chạy test · ai là owner |
| **Guides** (`docs/guides/`) | Người DÙNG framework | Tối thiểu 4 bài: bắt đầu từ số 0 · viết một adapter mới · thêm module quốc gia (fiscal) · chạy fleet simulator |
| **ADR** | Người hỏi "vì sao" | Như mục 8; bổ sung luật: **đổi `pos-ports`/`pos-proto` phải có ADR trước khi code** — áp dụng ngay từ bây giờ |

Luật xuyên suốt: **đổi hành vi = đổi tài liệu trong cùng PR** — checkbox bắt buộc trong PR template, reviewer gác phần máy không gác được. Changelog tự sinh từ conventional commits; bản phát hành nào đổi protocol/migration phải kèm ghi chú nâng cấp.

**Ngôn ngữ (quyết định có chủ đích):** code, rustdoc, commit message — **tiếng Anh** (chuẩn hệ sinh thái, AI thao tác chính xác hơn, mở đường contributor ngoài); guides & ADR — song ngữ hoặc Việt; runbook vận hành cho quán — **tiếng Việt**.

## 13. Người + AI cùng sửa code

**`AGENTS.md` ở gốc repo** — file ngữ cảnh mà mọi AI agent (và người mới) đọc đầu tiên, gồm đúng 5 phần:
1. Kiến trúc tóm trong 10 dòng (port/adapter, core thuần, cloud–edge).
2. **Luật cấm tuyệt đối:** không import hạ tầng vào core · không `unwrap` ngoài test · không thêm dependency mới khi chưa có ADR · không sửa `pos-proto` mà không xét PROTOCOL_VERSION · không log PII · không đụng `vendor/`, file snapshot API, hay khóa/secret.
3. **Bộ lệnh duy nhất:** `just preflight` (fmt + clippy + test + deny + public-api check) — AI không phải đoán cách build; chạy xanh mới được mở PR.
4. Definition of Done: code + test + rustdoc + docs liên quan + changelog scope.
5. Bản đồ thư mục: sửa loại việc X thì vào đâu.

**Kỷ luật PR (áp cho cả người lẫn AI, không ngoại lệ):** PR nhỏ một mục đích (hướng dẫn ≤ ~400 dòng thay đổi); template bắt buộc *what / why / how-tested / docs-updated*; **squash merge** để lịch sử tuyến tính (dễ bisect, dễ sinh changelog); PR có AI tham gia gắn nhãn `ai-assisted` — không để hạ chuẩn review mà để đo lường và truy vết; **người thật là người duy nhất bấm merge**, và CODEOWNERS bắt buộc review người cho core/ports/proto.

**Ba rủi ro riêng của code AI sinh ra + hàng rào tương ứng:**

| Rủi ro | Hàng rào |
|---|---|
| Nhiễm bản quyền (AI chép nguyên khối code không rõ nguồn) | Luật review: không nhận khối code lạ dài không kèm nguồn gốc/lý giải; `cargo-deny` không bắt được việc này — đây là việc của người |
| Bug tinh vi đúng-cú-pháp-sai-nghiệp-vụ | Lưới là **contract test + fleet simulator + đối soát đêm**, không phải mắt người đọc diff; AI muốn merge phải qua đúng lưới đó |
| Prompt injection nhắm vào agent (nội dung độc trong issue/code khiến agent làm bậy) | Agent chạy **không cầm secret**, quyền tối thiểu, không có quyền merge; tầng phát hành đã miễn nhiễm nhờ khóa ký offline (mục 6) |

## 14. Dễ sử dụng như một framework

- **`examples/` chạy được thật** (CI build chúng như code sản phẩm): `minimal-edge` — dựng một quán ảo bằng toàn fake adapter, chạy được ngay không cần phần cứng; `custom-printer` — tự viết một `PrinterDriver`; `fiscal-skeleton` — bộ khung module quốc gia mới.
- **`templates/adapter-template/`** (cargo-generate): lệnh một dòng sinh ra crate adapter mới có sẵn cấu trúc, import sẵn bộ contract test của port tương ứng — "thêm vendor" trở thành điền vào chỗ trống.
- **Hai bậc ổn định, tuyên bố công khai:** *Stable* = public API của core/ports/proto (semver + deprecation ≥ 2 bản); *Internal* = adapters, binaries, UI (đổi tự do). Người dùng framework chỉ nên phụ thuộc bậc Stable.
- **Definition of Done cho một adapter mới:** implement port → qua trọn bộ contract test → README theo template → ví dụ config → ADR nếu kéo theo dependency mới.

## 15. Quản trị & bảng rủi ro

| Rủi ro | Hàng rào |
|---|---|
| GitHub khóa/mất tài khoản | Mirror hằng ngày sang remote thứ hai (đã có, mục 5) |
| Supply-chain của GitHub Actions | **Pin mọi action theo commit SHA**, không theo tag; hạn chế action bên thứ ba |
| Bus factor = 1 | Tối thiểu **2 maintainer** có quyền phát hành + giữ khóa B; mọi quy trình sống trong runbook, không trong đầu ai |
| crates.io sập / crate biến mất | `vendor/` commit trong repo (đã có, mục 5) |
| Lộ secret qua repo/CI | Secret scanning + push protection (đã có); khóa ký không bao giờ ở CI (mục 6) |
| Code AI kém chất lượng trộn vào | Toàn bộ mục 13 |

Thêm hai file gốc: `MAINTAINERS.md` (ai giữ quyền gì) và `SECURITY.md` (báo lỗ hổng vào đâu). **Quy trình theo quy mô:** luật máy-thi-hành (mục 11–12) bật 100% từ commit đầu vì chi phí vận hành bằng 0; luật cần người vận hành — RFC đầy đủ cho thay đổi lớn, review SLA, coverage gate `pos-core` không-giảm — ghi sẵn và **kích hoạt khi > 3 người thường trực**. Ngoại lệ bật ngay: ADR-trước-khi-code cho ports/proto (mục 12).

## 16. Kế hoạch hoàn thiện (thay thế mục 10 làm lộ trình chính)

| Đợt | Việc | Kết quả |
|---|---|---|
| **Tuần 0–1** (trước dòng code sản phẩm đầu tiên) | 7 việc ở mục 10 + lint-as-code theo tầng (mục 11) + snapshot public API + `AGENTS.md` + `CONTRIBUTING.md` + PR/issue template + README template + pin Actions theo SHA | Repo mà cả người lẫn AI vào là biết luật, và luật tự thi hành |
| **Tháng đầu** (song song code sản phẩm) | Bộ contract test cho 3 port đầu (`EventStore`, `MessageLink`, `PrinterDriver`) · `examples/minimal-edge` · `templates/adapter-template` · 4 bài guides đầu · `MAINTAINERS.md`/`SECURITY.md` | Người ngoài (hoặc AI) viết được adapter đầu tiên mà không cần hỏi ai |
| **Khi > 3 người thường trực** | RFC process đầy đủ · review SLA · coverage gate `pos-core` không-giảm · mở rộng CODEOWNERS | Quy trình lớn bật đúng lúc cần, không sớm hơn |

**Điều chỉnh sau vòng rà soát chống dư thừa:** adapter-template chưng cất khi viết adapter **thứ ba** (rule of three, không đoán cấu trúc trước khi có cái lặp lại); 4 bài guides viết dần khi state machine + 2 adapter đầu tồn tại; fleet simulator và e2e fork-to-UI tự động là **điều kiện của cổng "lên 50 store"**, không phải việc tháng đầu. Nguyên tắc chung: hạ tầng kiểm thử mở rộng *theo* fleet thật, không đi trước nó. Bộ tài liệu 4 file đóng băng làm baseline — sản phẩm tiếp theo là code (bắt đầu từ state machine `pos-core`, Kiến trúc 15.5).

Thước đo "hoàn thiện" cuối cùng của cả kế hoạch này: **một contributor mới — người hoặc AI — nhận issue "viết adapter cho máy in hãng X", và tự đi từ số 0 đến PR đạt chuẩn chỉ bằng những gì có trong repo.** Khi điều đó xảy ra, đây thật sự là một framework.


---

# PHẦN III — Trải nghiệm fork-and-deploy

## 17. Từ fork đến giao diện: ~15 phút, không chạm console server

### 17.1 Luồng chuẩn

```
Fork repo → nhập Secrets (17.2) → tab Actions → "Deploy" → Run
   │
   ▼
GitHub Action (deploy.yml):
  1. Build image theo tag → nén thành file (~50–100MB)
  2. SSH vào VPS bằng secret
  3. bootstrap.sh — IDEMPOTENT, chạy lại bao nhiêu lần cũng an toàn:
     • cài Docker nếu chưa có
     • SINH secret vận hành TẠI CHỖ (mật khẩu DB, NATS seed, khóa S3)
       → lưu /opt/pos/secrets trên VPS, KHÔNG quay về GitHub
     • docker compose up: pos_cloud · Postgres · NATS · Garage
       (+ profile "monitoring": VictoriaMetrics · Grafana — tùy chọn)
     • Caddy tự xin & tự gia hạn TLS Let's Encrypt cho DOMAIN
  4. In ra Summary: URL + mã setup DÙNG MỘT LẦN (hết hạn 24h)
   │
   ▼
Mở https://admin.DOMAIN/setup?token=…  → tạo SIÊU QUẢN TRỊ (TOTP bắt buộc)
→ endpoint /setup tự khóa vĩnh viễn sau tài khoản đầu tiên
→ tạo Tenant "abc" → hệ thống sinh https://abc.DOMAIN + link mời Tenant Admin
→ Tenant Admin vào subdomain: Brand → Store (sinh kèm MENU MẪU
   + checklist sẵn sàng bán) → bấm Export
→ nhận file cài + mã kích hoạt → mang ra quán chạy (Kiến trúc mục 5)
```

### 17.2 Secrets & Environments

| Secret | Bắt buộc? | Dùng làm gì |
|---|---|---|
| `VPS_HOST` | ✓ | IP / hostname máy chủ |
| `VPS_SSH_PORT` | ✓ (mặc định 22) | Cổng SSH |
| `VPS_USER` | ✓ | User có sudo |
| `VPS_SSH_KEY` | ✓ | Private key SSH, tạo riêng chỉ để deploy |
| `DOMAIN` | khuyến nghị | Tên miền trỏ về VPS — chưa có domain xem 17.8 |
| `ACME_EMAIL` | khuyến nghị | Email nhận thông báo Let's Encrypt |
| `RCLONE_REMOTE_*` | tùy chọn | Đích backup lớp 2 (Kiến trúc 14.5) |
| `CF_DNS_API_TOKEN` | tùy chọn — **bắt buộc khi bật đa tenant subdomain** | Token Cloudflare (scope Zone:DNS:Edit) cho ACME DNS-01 — đóng cổng 80, cấp wildcard cert `*.domain` (17.5, 17.10) |

**Ranh giới tin cậy của setup token:** token in trong Action Summary — ai có quyền *đọc* repo đều xem được cho tới khi nó được dùng hoặc hết hạn 24h. Với repo private ít người, chấp nhận được (token một lần + `/setup` tự khóa vĩnh viễn); chỉ cần biết rõ: **quyền đọc repo = quyền setup lần đầu**. Đừng để repo public trước lần setup.

Deploy chạy trong **GitHub Environment `production`** (bật required reviewer khi > 1 người có quyền). Muốn staging: thêm Environment thứ hai với bộ secret khác — dùng chung một workflow.

### 17.3 Nguyên tắc secret hai tầng

GitHub Secrets chỉ giữ **chìa khóa vào cửa**: đường SSH tới VPS + thông tin công khai (domain, email). Secret **vận hành** — mật khẩu Postgres, NATS operator seed, khóa Garage — do `bootstrap.sh` sinh ngẫu nhiên *ngay trên VPS* ở lần chạy đầu và ở lại đó vĩnh viễn. Vì sao không nhét hết vào GitHub: (a) người có quyền admin repo không mặc nhiên nắm production DB; (b) xoay secret vận hành không cần đụng GitHub hay redeploy; (c) fork/chia sẻ repo không bao giờ rò secret hệ thống. Trải nghiệm không đổi — vẫn chỉ nhập 4–6 secret rồi bấm một nút.

### 17.4 Cái gì chạy trên VPS sau khi deploy

Compose ghim image theo **digest**: `pos_cloud` (build từ chính repo), Postgres, NATS, Garage — bản tối thiểu 4 container, ~1.2–1.5GB RAM, vừa VPS nhỏ nhất. Profile `monitoring` (VictoriaMetrics + Grafana) bật thêm khi cần. Toàn bộ dữ liệu nằm trong volume — compose down/up không mất gì.

Ghi chú trung thực: Docker là **lần duy nhất** kiến trúc chủ động thêm một phụ thuộc nền (~100–200MB) — đổi lấy việc bootstrap chạy giống hệt nhau trên mọi VPS Ubuntu/Debian, thay vì script cài 4 dịch vụ native mong manh theo từng distro. Các service đều là binary tĩnh nên nếu về sau muốn tuyệt đối tối giản, chuyển sang systemd thuần là một ADR riêng — không phải viết lại gì.

### 17.5 TLS hai giai đoạn — và luật khi DNS nằm ở Cloudflare

Giai đoạn 1: **Caddy** làm reverse proxy — tự xin, tự gia hạn Let's Encrypt, cấu hình 5 dòng, 0 đồng. Giai đoạn 2 (đúng chiến lược Phụ lục D — bỏ giao thức là bỏ cả tầng): tích hợp ACME thẳng vào `pos_cloud` (crate `rustls-acme`), Caddy biến mất, cloud lại về đúng một binary + các dịch vụ dữ liệu.

**Khi DNS quản lý trên Cloudflare — ba luật:**

1. **Mọi record để "DNS only" (đám mây xám).** Bật proxy (đám mây cam) nghĩa là Cloudflare giải mã toàn bộ traffic ở edge — quay lại đúng cuộc debate tunnel (PII, phụ thuộc, SPOF); nếu ngày nào đó bật cam thì phải theo quyết định ADR về tunnel, và chế độ SSL bắt buộc là "Full (Strict)".
2. **Cấm tuyệt đối chế độ SSL "Flexible"** của Cloudflare: trình duyệt thấy ổ khóa nhưng đoạn Cloudflare → VPS chạy HTTP trần qua internet — an toàn giả, tệ hơn không có SSL.
3. **Tùy chọn đáng giá — ACME DNS-01 qua API Cloudflare** (secret `CF_DNS_API_TOKEN`, scope Zone:DNS:Edit): Caddy xin cert bằng record TXT thay vì cổng 80 → đóng được cổng 80 trên VPS, và cấp **wildcard `*.domain`** — mở đường mỗi tenant một subdomain sau này mà không xin cert từng cái. Token này là secret vận hành: bootstrap nhận từ GitHub Secret rồi lưu tại `/opt/pos/secrets` cho Caddy tự gia hạn (nguyên tắc 17.3).

NATS (4222) không dùng chứng chỉ công cộng — đã có mTLS với khóa tự quản riêng.

### 17.6 Nâng cấp & rollback — một nút cho cả hai chiều

Nâng cấp = chạy lại workflow với tag mới: kéo image mới → migration → khởi động lại (downtime vài giây; các quán không ảnh hưởng — offline-first). Rollback = chạy lại workflow với tag cũ. Không tồn tại bước tay nào trên server ở cả hai chiều.

### 17.7 GKE / Kubernetes — làn tùy chọn

Cùng image container chạy được trên GKE qua manifest `deploy/k8s/` (secrets qua Environment: `GCP_SA_KEY`, `GKE_CLUSTER`…). Nói thẳng: GKE là dịch vụ **có phí**, lệch ràng buộc chi phí ≈ 0 của kiến trúc — hỗ trợ cho ai đã có hạ tầng GCP; khuyến nghị mặc định vẫn là VPS.

### 17.8 Việc tay duy nhất còn lại — và mẹo bỏ luôn nó

Ngoài GitHub chỉ còn một việc tay: trỏ DNS A record của DOMAIN về IP VPS. Chưa có domain? Đặt `DOMAIN = <ip-vps>.sslip.io` — DNS công cộng miễn phí tự phân giải về đúng IP nằm trong tên, Let's Encrypt cấp chứng chỉ bình thường → **không cần mua domain vẫn có HTTPS**.

### 17.9 Thước đo hoàn thiện thứ hai

Cùng với thước đo adapter ở mục 16: **một người chưa từng đọc tài liệu, chỉ với một VPS trắng + repo này, đi từ fork đến màn hình tạo Tenant/Brand/Store trong ≤ 15 phút, không gõ lệnh nào trên server.** Kiểm chứng: diễn tập tay theo checklist mỗi bản phát hành lớn; tự động hóa thành e2e dựng VPS ảo là hạng mục của cổng ">50 store" — tự động hóa sớm hơn là mạ vàng.

### 17.10 Đa tenant theo subdomain & siêu quản trị

**Hai tầng vai trò quản trị:**

| | Siêu quản trị (platform) | Tenant Admin |
|---|---|---|
| Được tạo | Một lần duy nhất qua `/setup?token=…` sau deploy — **TOTP bắt buộc ngay tại bước tạo** | Siêu quản trị tạo/mời khi lập tenant |
| Sống ở | `admin.DOMAIN` — console nền tảng | `<slug>.DOMAIN` — chỉ tenant của mình |
| Quyền | Tạo/khóa tenant, cấu hình nền tảng, heatmap fleet toàn hệ thống, danh sách khóa OTA thu hồi | Toàn quyền trong tenant: brand, store, nhân sự, menu, export file cài |

**An toàn bước setup:** `/setup` tự khóa vĩnh viễn sau khi siêu quản trị đầu tiên tồn tại · token mất/quá 24h → chạy lại workflow, in token mới (bootstrap idempotent, không cần SSH) · siêu quản trị mất mật khẩu + TOTP → workflow input `reset_admin=true` in recovery token dùng-một-lần (break-glass duy nhất, nằm sau quyền GitHub Environment).

**Mời người không cần SMTP:** hệ thống không gửi mail (tránh thêm phụ thuộc); mọi lời mời là **link có hạn** sinh trên dashboard, admin tự gửi qua Zalo/email; người nhận bấm link tự đặt mật khẩu.

**Cơ chế subdomain:**
1. DNS: một record wildcard `*.DOMAIN` → VPS, **đám mây xám**. Toàn bộ luồng (DNS hosting, wildcard record, API token cho DNS-01) nằm trong **gói Free** của Cloudflare — cert do Let's Encrypt cấp miễn phí. Lưu ý: proxied wildcard cũng đã có ở gói Free (từ 2022), nhưng giữ xám là quyết định kiến trúc (không cho bên thứ ba giải mã dữ liệu — xem debate tunnel), không phải vì giá. Nếu có ngày bật cam: Universal SSL miễn phí chỉ phủ wildcard một cấp — slug một cấp của thiết kế này vẫn nằm trong vùng miễn phí.
2. Cert: wildcard chỉ xin được qua **DNS-01** → `CF_DNS_API_TOKEN` bắt buộc khi bật tính năng này; một cert `*.DOMAIN` phủ mọi tenant — tạo tenant mới không cần xin cert.
3. Routing: `pos_cloud` đọc Host header → tra slug → set tenant context → RLS cách ly dữ liệu (tái dùng cơ chế partition + RLS sẵn có). Slug: `a-z0-9-`, 3–30 ký tự, chặn danh sách dành riêng (`admin`, `api`, `www`, `setup`, `status`, `assets`…).
4. **Cookie phiên đặt theo từng subdomain**, không bao giờ ở `.DOMAIN` — tránh phiên tenant này gửi sang tenant kia (lỗ cách ly nghiêm trọng nhất của multi-tenant).

Các quán **không ảnh hưởng**: edge nói chuyện qua NATS, không qua HTTP subdomain — bật đa tenant không đổi một dòng config nào ở 1000 quán.

Các hạng mục của mục 17 (`deploy/`, `deploy.yml`, `bootstrap.sh`, wizard setup siêu quản trị + tenant subdomain + export) xếp vào đợt **"Tháng đầu"** của lộ trình mục 16.

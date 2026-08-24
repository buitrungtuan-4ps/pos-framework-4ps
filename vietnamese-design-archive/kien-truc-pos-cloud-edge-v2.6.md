# Hệ thống POS Cloud–Edge — Tài liệu kiến trúc v2.6 (bản chốt)

> **Phân loại:** T3 – Nội bộ (có yếu tố T2: hạ tầng, điều khoản vendor). Rà soát trước khi gửi ra ngoài tổ chức.
> **Ngày:** 13/08/2026 · Thay thế toàn bộ bản v1, v2, v2.1, v2.2, v2.3, v2.4, v2.5.
> **Tài liệu đi kèm:** Đặc tả nghiệp vụ POS v1 (`dac-ta-nghiep-vu-pos-v1.md`) · Kế hoạch quản lý code trên GitHub (`ke-hoach-quan-ly-code-github.md`) · UI/UX guideline (`ui-ux-guideline-v1.md`) · Chuẩn đặt tên & API (`chuan-dat-ten-va-api-v1.md`) · Báo cáo khả thi, tải & phục hồi (`bao-cao-kha-thi-tai-va-phuc-hoi-v1.md`).
> **Quy mô tham chiếu:** 50 store (thiết kế sẵn đường lên 1000). Cloud ví dụ: 1 VPS Linux 4 core Ampere / 24GB RAM / 100GB.

---

## 0. Hệ thống này phải đạt gì

Cắm là chạy (plug and play), nhân viên không rành kỹ thuật tự cài được. Quán bán hàng bình thường khi mất mạng, và chạy tốt sau 4G — không cần public IP hay IP tĩnh. Máy hỏng thì thay máy khác, dữ liệu kéo từ cloud về, 5–10 phút bán tiếp. Mọi phản hồi trong quán dưới 100ms (thực tế đạt dưới 5ms), truy vấn cloud dưới 100ms, phần xử lý của mình khi nối vendor dưới 300ms. Chi phí phần mềm bằng 0 — toàn bộ là mã nguồn mở tự vận hành; hai ngoại lệ không né được: phí nhà cung cấp hóa đơn điện tử (nghĩa vụ pháp lý, tính theo hóa đơn) và SIM 4G (chi phí viễn thông). License thiết bị (Windows) nằm ngoài phạm vi tính chi phí. Và một yêu cầu xuyên suốt: **mọi cấu hình — từ port/domain của máy chủ quán tới danh sách nhân viên, phân quyền, thời gian offline cho phép, license thiết bị, máy in, KDS, món, category, layout — đều quản trị tập trung trên cloud**; tại quán không chỉnh tay bất kỳ file nào, và file cài đặt luôn là bản mới nhất kéo từ cloud.

---

## 1. Bức tranh tổng thể

Toàn bộ kiến trúc tóm trong 4 câu:

1. **Quán tự chạy 100%** — cloud chỉ để quản trị (tenant → brand → store) và đồng bộ dữ liệu, không nằm trên đường đi của bất kỳ giao dịch nào tại quán.
2. **Mỗi tầng là MỘT chương trình duy nhất** — cloud là một binary `pos_cloud`, quán là một binary `pos_edge`. Không có rừng service.
3. **Máy nào chết thì thay, không cứu** — dữ liệu quán luôn có bản sao liên tục trên cloud.
4. **Mọi cấu hình sống trên cloud** — quán chỉ nhận và áp dụng; không có file nào để chỉnh tay tại máy quán (chi tiết ở mục 7.1).

```
        Grab / ShopeeFood        SAP ERP        NCC hóa đơn điện tử ──► Cơ quan thuế
               │                    │                    │
               ▼          webhook / API          ▼       ▼
┌──────────────────────────────────────────────────────────────────────┐
│  CLOUD — 1 VPS Linux (ví dụ: 4 core / 24GB / 100GB)                  │
│                                                                      │
│  ┌────────────────────────────────┐      ┌───────────────────────┐   │
│  │  pos_cloud  (1 binary Rust)    │◄────►│  PostgreSQL           │   │
│  │  · Cổng API + auth (tự viết)   │      │  partition theo store │   │
│  │  · Hub vendor (các adapter)    │      │  + bảng rollup        │   │
│  │  · Fleet: OTA, license/lease   │      └───────────────────────┘   │
│  │  · Nhận sự kiện từ các store   │      ┌───────────┐ ┌─────────┐   │
│  └───────────────┬────────────────┘      │ MinIO (S3)│ │ Metrics │   │
│                  │                       │ backup +  │ │ Grafana │   │
│  ┌───────────────▼────────────────┐      │ bản OTA   │ └─────────┘   │
│  │  NATS JetStream (hàng đợi bền) │      └───────────┘               │
│  └───────────────┬────────────────┘                                  │
└──────────────────┼───────────────────────────────────────────────────┘
                   │   Store TỰ QUAY SỐ RA (outbound TLS)
                   │   → chạy sau 4G / CGNAT, không cần public IP
       ┌───────────┼─────────────┬─────────────────┐
       ▼           ▼             ▼                 ▼
   Store #1    Store #2      Store #3    ...   Store #50
```

Bên trong một store:

```
┌─ STORE ─────────────────────────────────────────────────────────────┐
│                                                                     │
│  MÁY CHỦ QUÁN: một máy bất kỳ cắm điện cố định (Windows hoặc Linux) │
│  ┌───────────────────────────────────────────────────┐              │
│  │  pos_edge  (1 binary, chạy ngầm như service)      │              │
│  │  · Web UI nhúng sẵn      · SQLite (giữ 90 ngày)   │              │
│  │  · Đồng bộ outbox        · Hóa đơn điện tử offline│              │
│  │  · Tự cập nhật (OTA)     · In / két / máy cà thẻ  │              │
│  │  · Litestream ────────► backup liên tục lên cloud │              │
│  └────────────────────┬──────────────────────────────┘              │
│                       │  LAN — địa chỉ cố định: pos.local           │
│      ┌────────────────┼────────────────┬────────────────┐           │
│      ▼                ▼                ▼                ▼           │
│  Màn thu ngân    iPad / điện thoại   Màn bếp (KDS)   Máy in ESC/POS │
│  (trình duyệt)   (trình duyệt)       (trình duyệt)   két, cà thẻ    │
└─────────────────────────────────────────────────────────────────────┘
```

Ví dụ thiết bị thực tế cho một store: PC Windows làm máy chủ (cắm điện cố định), điện thoại và MacBook mở trình duyệt là thành máy order / màn thu ngân — không cài thêm gì.

---

## 2. Mỗi mục tiêu được giải bằng gì (bảng tra nhanh)

| Mục tiêu | Cơ chế | Xem mục |
|---|---|---|
| Plug and play | 1 file cài + 1 mã kích hoạt; tự quét máy in; client chỉ cần trình duyệt | 5 |
| Quản trị tập trung 100% | Cây cấu hình trên cloud, sync xuống < 1 giây; quán không có file chỉnh tay | 7.1 |
| Tài nguyên tối thiểu | 1 binary mỗi tầng; cắt mọi thành phần không cần (danh sách ở mục 12) | 3, 12 |
| Hiệu năng tối đa | Rust + SQLite WAL tại quán; bảng rollup trên cloud; số liệu cụ thể | 11 |
| Xử lý sự cố cực nhanh | Heatmap + cảnh báo; kéo log trực tiếp từ store; thay máy 5–10 phút | 3.5, 6 |
| Chỉ có 4G, không public IP | Mọi kết nối do store quay số RA; không mở port, không IP tĩnh | 3.2 |
| Khôi phục từ cloud | Backup liên tục từng giây (Litestream); máy mới tự kéo về khi kích hoạt | 6 |
| Nhân viên không tech cài được | Đúng 2 thao tác: chạy file, gõ mã | 5 |
| Latency trong quán < 100ms | Thực tế < 5ms; kỹ thuật ở 4.3 | 11 |
| Vendor < 300ms (phần mình) | Gọi trong cùng 1 process + ghi nhận rồi trả lời; đo riêng từng adapter | 3.1, 8 |
| Query DB < 100ms | Partition + bảng rollup; dashboard đọc bảng nhỏ < 10ms | 3.3 |
| Chi phí phần mềm ≈ 0 | 100% mã nguồn mở tự vận hành (ngoại lệ ở mục 0) | toàn bộ |

---

## 3. Tầng cloud

### 3.1 `pos_cloud` — một chương trình duy nhất (modular monolith)

Các khối chức năng — cổng API, hub vendor, quản lý fleet, xác thực — là các **module trong cùng một chương trình**, gọi nhau như gọi hàm (nano giây), không đi qua mạng. So với tách thành nhiều service: nhanh hơn (không có bước nhảy mạng giữa các khối — phục vụ trực tiếp mục tiêu < 300ms), nhẹ hơn (một runtime thay vì năm), và chỉ có một thứ để chạy, theo dõi, khởi động lại. Cái giá chấp nhận: lỗi nặng làm sập cả chương trình — hóa giải bằng supervisor tự khởi động lại trong ~1 giây, và mọi dữ liệu vào đều đã nằm an toàn trong hàng đợi NATS nên khởi động lại không rơi gì. Chỉ tách một module ra riêng khi có bằng chứng nó cần tải khác hẳn — hiện chưa có module nào như vậy.

**Xác thực (tự viết):** cloud chỉ quản lý vài trăm tài khoản quản trị theo cây Tenant → Brand → Store. Email + mật khẩu (băm Argon2) + TOTP, viết bằng thư viện chuẩn ngay trong `pos_cloud`. Không Firebase, không Keycloak.

**Webhook từ vendor:** kiểm chữ ký → ghi ngay vào hàng đợi bền trên NATS (chỉ xác nhận với vendor sau khi đã ghi xong) → xử lý sau đó với cơ chế thử lại và hộp thư lỗi (DLQ). Cloud khởi động lại giữa chừng không mất đơn nào.

### 3.2 NATS — câu trả lời cho "quán chỉ có 4G, không public IP"

NATS là trạm trung chuyển tin nhắn. Điểm mấu chốt: **store luôn là bên gọi ra** — nó mở một kết nối TLS lên cloud và giữ kết nối đó. Cloud muốn gửi gì xuống (menu mới, lệnh cập nhật, yêu cầu xem log) đều đi ngược qua đường ống store đã mở sẵn. Vì vậy quán không cần mở port, không cần IP tĩnh, không cần tunnel trả phí — 4G sau CGNAT của nhà mạng chạy bình thường. Mất mạng thì tin nhắn xếp hàng ở hai đầu, có mạng lại là tự chảy tiếp.

Trên một VPS đơn chạy **1 node** NATS (cluster 3 node chỉ có nghĩa khi có từ 3 máy trở lên). Chấp nhận được: cloud chết thì không quán nào ngừng bán, và hàng đợi lưu trên đĩa nên khởi động lại là dữ liệu còn nguyên.

### 3.3 PostgreSQL — kho dữ liệu trung tâm

- **Một schema chung, partition theo `store_id`** + Row-Level Security để cách ly tenant. Thuộc tính linh hoạt của món (topping, modifier…) dùng cột JSONB.
- **Bảng rollup** = bảng tổng hợp sẵn theo giờ/ngày/store, cập nhật tăng dần khi sự kiện đổ về. Dashboard và báo cáo chỉ đọc các bảng nhỏ này → luôn dưới 10ms, không bao giờ quét bảng đơn hàng gốc. Đây là cách đạt "query < 100ms" mà không cần thêm hệ thống phân tích nào.
- **Kết nối:** chỉ có `pos_cloud` nối vào Postgres, với pool nội bộ 20–50 kết nối cố định. 50 (hay 1000) store **không bao giờ** nối thẳng DB — chúng nói chuyện qua NATS. Vì vậy không có "vấn đề connection" để giải, và không cần PgBouncer.
- **Backup:** WAL archiving đẩy **ra khỏi máy** (snapshot của nhà cung cấp VPS, hoặc một VPS lưu trữ rẻ — nằm trong ngân sách VPS cho phép). Backup nằm cùng đĩa với DB thì không phải backup.

### 3.4 MinIO — kho file kiểu S3, tự vận hành, 0 đồng

Một binary mã nguồn mở chạy ngay trên VPS, lưu ba thứ: (a) **bản sao SQLite của từng store** do Litestream đẩy lên liên tục — đây chính là thứ cho phép "thay máy 5–10 phút"; (b) **bản cài OTA** (mỗi bản 20–50MB); (c) file xuất/báo cáo. Vì store chỉ giữ 90 ngày dữ liệu, mỗi bản sao nén chỉ 30–80MB → 50 store chiếm ~5–10GB.

### 3.5 Giám sát & xử lý sự cố

Store đẩy lên cloud **chỉ số đo + lỗi/cảnh báo** (vài chục KB/ngày mỗi store). Log chi tiết đầy đủ nằm tại máy quán, xoay vòng 7–14 ngày. Khi cần điều tra store #23, kỹ sư gửi lệnh qua NATS "cho xem 30 phút log gần nhất" — store trả về đúng lát cắt đó, như tail từ xa. Không đổ hàng chục GB log/ngày qua 4G để rồi không ai đọc.

Cảnh báo tối thiểu: store offline > 5 phút · hàng đợi hóa đơn điện tử tồn đọng · dải số hóa đơn sắp cạn · đĩa < 10% · lệch giờ vượt ngưỡng · lỗi in tăng đột biến. Dashboard heatmap xanh/đỏ toàn chuỗi. Ở quy mô ≤ 50 quán có thể khởi đầu **không bật profile monitoring**: số đo tần suất thưa (60–120 giây, ~15 chỉ số) ghi thẳng Postgres (~20–40MB/ngày, retention 30 ngày) và vẽ trên trang trạng thái hệ thống; VictoriaMetrics + Grafana bật khi vượt ~100 quán hoặc cần độ phân giải 15–30 giây — không mâu thuẫn Phụ lục D vì cảnh báo ở đó áp cho metrics độ phân giải cao.

---

## 4. Tầng quán (edge)

### 4.1 Phần cứng & hệ điều hành

Một máy bất kỳ làm máy chủ quán: PC Windows, mini PC, hoặc máy Linux — **Windows và Linux đều là lựa chọn hạng nhất** (một codebase Rust build ra cả hai). Khuyến nghị: máy cắm điện cố định, RAM ≥ 8GB, ổ SSD, tắt auto-update của OS (bản IoT/LTSC càng tốt để máy không tự khởi động lại giữa giờ bán). Mã hóa ổ đĩa: BitLocker (Windows) / LUKS (Linux). Giờ hệ thống đồng bộ NTP về cloud.

Ghi chú duy nhất về khác biệt OS: driver máy cà thẻ dạng DLL chỉ có trên Windows — quán nào dùng loại đó thì máy chủ quán chạy Windows, hoặc dùng giao thức TCP/serial của terminal nếu có (đa nền tảng). Ngoài điểm này, hai OS giống hệt nhau.

### 4.2 `pos_edge` — một chương trình, chạy ngầm

Đăng ký chạy như Windows Service / systemd, tự khởi động cùng máy. Bên trong:

- **Web UI nhúng sẵn** trong binary (rust-embed) — phục vụ mọi màn hình trong LAN, không cần cài web server.
- **Đồng bộ (outbox):** xem mục 7.
- **Hóa đơn điện tử offline:** phát hành từ dải số cloud cấp trước, xếp hàng đợi khi mất mạng, đẩy bù khi có mạng (mục 8).
- **Tự cập nhật (OTA):** mục 9.
- **Ngoại vi:** in ESC/POS đa hãng, chia phiếu bếp theo trạm, kích két, nối máy cà thẻ (kèm mã chống trùng + nhật ký trạng thái + đối soát cuối ngày).
- **Litestream:** đẩy từng thay đổi của SQLite lên MinIO trên cloud, độ trễ vài giây.

### 4.3 SQLite & hiệu năng tại quán

SQLite chạy WAL mode, chỉ giữ **90 ngày** dữ liệu (kho lưu trữ dài hạn là Postgres trên cloud) → file DB ổn định 100–300MB, backup nhẹ, khôi phục nhanh. Bộ kỹ thuật bắt buộc để "luôn dưới 100ms" (thực tế dưới 5ms): `synchronous=NORMAL` + `busy_timeout`; prepared statements; **mỗi màn hình đúng một câu query** vào bảng đọc phẳng dựng sẵn; index phủ cho các truy vấn nóng. Năng lực nền: hơn 10.000 giao dịch ghi/giây — nhu cầu giờ cao điểm của một quán là 1–3/giây.

### 4.4 Client = trình duyệt, không app riêng

Điện thoại, iPad, Mac, PC — mở trình duyệt tới `pos.local` là thành thiết bị POS. Không có app phải build cho từng nền tảng, không có kênh update client riêng (UI mới đi cùng bản OTA của máy chủ quán). Khóa màn hình thu ngân bằng tính năng có sẵn của OS: Assigned Access (Windows), Guided Access (iPadOS). Ghép thiết bị lần đầu bằng quét QR để nhận token — thiết bị lạ trong LAN không gọi được API.

---

## 5. Cài đặt — nhân viên làm đúng 2 thao tác

```
  ADMIN (dashboard)              NHÂN VIÊN TẠI QUÁN                CLOUD
  ─────────────────              ──────────────────                ─────
  1. Tạo store
     → nhận MÃ KÍCH HOẠT ─Zalo─► 2. Tải 1 file cài, chạy
                                 3. Gõ mã kích hoạt ─────────────► 4. Kiểm tra mã
                                                                      cấp giấy phép (lease)
                                                                      + dải số hóa đơn
                                 5. Máy tự kéo cấu hình, menu ◄─────┘
                                    (nếu là THAY MÁY: kéo cả
                                     toàn bộ dữ liệu backup)
                                 6. Tự công bố pos.local trên LAN
                                    tự quét máy in (cổng 9100/USB)
                                 7. Dashboard chuyển XANH — xong
```

**File cài luôn là bản mới nhất:** file tải về là một bootstrapper nhỏ (vài MB) — khi chạy, nó kéo bản `pos_edge` mới nhất từ cloud, kiểm chữ ký rồi mới cài. Nhờ vậy kể cả file cài lưu trên USB từ tháng trước vẫn cài ra đúng bản hiện hành; sau kích hoạt, OTA (mục 9) giữ máy luôn cập nhật. (Cài đặt cần mạng — vốn hiển nhiên vì bước kích hoạt cũng cần mạng.)

Mã kích hoạt dùng một lần rồi vô hiệu; sau kích hoạt máy giữ credential dài hạn trong kho khóa của OS (TPM/DPAPI trên Windows, keyring trên Linux). Không còn file cấu hình chứa bí mật đi kèm bản cài — an toàn hơn và đơn giản hơn cho người cài.

**Ghép thiết bị & đường lùi nhập tay:** QR ghép thiết bị chứa thẳng đường link `http://<IP máy chủ>/pair?...` — quét bằng camera có sẵn của điện thoại, bấm là xong (né hai bẫy thực tế: Chrome trên Android không phân giải mDNS, và camera trong web đòi HTTPS); IP máy chủ được ghim bằng DHCP reservation trên router ngay bước cài. Mất QR hoặc mất token: màn hình client luôn có nút **Nhập tay** — gõ IP:port (hoặc `pos.local` với thiết bị hỗ trợ) + mã ghép 6 số đang hiển thị trên màn hình máy chủ hoặc dashboard. Admin thu hồi và cấp lại token thiết bị từ dashboard bất kỳ lúc nào. Nhập tay là đường cứu hộ để *kết nối*, không phải chỗ chỉnh cấu hình — port/domain chính thức vẫn do cloud quyết (mục 7.1).

---

## 6. Sự cố & khôi phục — triết lý "máy chết thì thay"

Không có máy dự phòng chuyên dụng, không có chế độ khẩn cấp riêng, không cứu máy hỏng. Quy trình duy nhất:

```
  Máy chủ quán chết
        │
        ▼
  Lấy MỘT MÁY BẤT KỲ (Windows/Linux) → chạy file cài + gõ mã kích hoạt mới
        │
        ▼
  Cloud thu hồi quyền của máy cũ (lease) + cấp DẢI SỐ HÓA ĐƠN MỚI
        │
        ▼
  Máy mới tự kéo: cấu hình + toàn bộ dữ liệu (bản sao mới nhất từ MinIO)
        │
        ▼
  iPad / màn hình tự nối lại qua pos.local  →  BÁN TIẾP (tổng ~5–10 phút)
```

**Lease — giấy phép "một quán chỉ một máy chủ":** giải quyết đúng một tai nạn có thật: máy A đơ, kích hoạt máy B thay, rồi máy A tự sống lại — nếu không có lease thì hai máy cùng nhận đơn và **cùng phát hành hóa đơn, tệ nhất là trùng số hóa đơn (lỗi pháp lý)**. Với lease: B kích hoạt là quyền của A bị thu hồi và B nhận dải số hóa đơn mới (kể cả có khoảng chồng lấn cũng không thể trùng số); A vừa có mạng lại là biết mình mất quyền → tự chuyển chế độ chỉ-đọc, các đơn nó lỡ ghi vẫn đồng bộ lên bình thường. Quan trọng: lease **không hết hạn khi offline** — máy đang giữ quyền chạy không mạng vô thời hạn; tranh chấp chỉ xảy ra tại thời điểm kích hoạt máy mới.

Trong lúc thay máy quán ngừng bán vài phút — chấp nhận có chủ đích, đổi lại toàn bộ hệ thống không phải nuôi bất kỳ cơ chế dự phòng nóng nào tại quán.

---

## 7. Dữ liệu, cấu hình & đồng bộ — 3 luật

```
        CLOUD là nguồn chân lý                STORE là nguồn chân lý
  ┌───────────────────────────┐         ┌───────────────────────────┐
  │ Toàn bộ CẤU HÌNH (7.1):   │ ──────► │ áp dụng < 1 giây,         │
  │ menu, giá, nhân sự,       │ ──────► │ không cần tải lại trang   │
  │ quyền, thiết bị, máy in,  │         │                           │
  │ KDS, layout, chính sách…  │         │                           │
  │ Dải số hóa đơn            │ ──────► │ phát hành offline được    │
  │ Điều chỉnh tồn kho        │ ──────► │                           │
  │                           │ ◄────── │ Đơn · thanh toán · ca     │
  │ ghi đúng 1 lần nhờ ULID   │ ◄────── │ Sự kiện tiêu hao tồn kho  │
  │ Đối soát hóa đơn          │ ◄────── │ Hóa đơn đã phát hành      │
  │ (đề xuất — admin duyệt)   │ ◄────── │ Máy in/KDS phát hiện được │
  └───────────────────────────┘         └───────────────────────────┘
```

1. **Mỗi loại dữ liệu có đúng một nơi được quyền quyết định.** Menu/giá: cloud quyết. Đơn hàng/ca: quán quyết. Không bao giờ có hai bên cùng sửa một thứ → xung đột không thể xảy ra, thay vì "xảy ra rồi xử lý".
2. **Chỉ gửi sự kiện, không đồng bộ trạng thái.** Ví dụ tồn kho: quán gửi "đã tiêu hao X", cloud gửi "đã nhập thêm Y" — số tồn ở mỗi nơi là kết quả cộng dồn từ các sự kiện, hai bên không bao giờ ghi đè nhau. Kỹ thuật **outbox**: sự kiện được ghi *trong cùng giao dịch SQLite* với đơn hàng, rồi một tiến trình nền gửi đi và thử lại đến khi cloud xác nhận — máy crash hay rớt 4G giữa chừng không làm mất sự kiện nào.
3. **ID sinh tại quán bằng ULID** (chuỗi định danh duy nhất có kèm mốc thời gian, sinh cục bộ không cần hỏi ai). 50 quán offline cùng sinh ID không bao giờ trùng, và vì gửi lại vẫn là ID cũ nên cloud nhận trùng bao nhiêu lần cũng chỉ ghi một.

Đồng hồ máy quán đồng bộ NTP; heartbeat kèm kiểm tra lệch giờ, lệch vượt ngưỡng là cảnh báo (bảo vệ thứ tự ID, ca làm việc, khung giờ khuyến mãi).

### 7.1 Cây cấu hình — mọi thứ trừ giao dịch đều là cấu hình, và đều nằm trên cloud

Tại quán **không có file cấu hình nào để chỉnh tay**. Mọi thứ chỉnh trên dashboard, đi xuống bằng đúng một cơ chế (gói delta có version, áp dụng < 1 giây khi online); quán luôn giữ một bản chụp đầy đủ trong SQLite nên mất mạng vẫn chạy với cấu hình đã biết cuối cùng, bản cập nhật tự đến khi có mạng lại.

Cấu hình xếp theo cây kế thừa **Tenant → Brand → Store → Thiết bị**: đặt ở tầng trên thì tầng dưới hưởng theo, tầng dưới ghi đè được khi cần (ví dụ: menu chung toàn brand, riêng một store ẩn vài món và cộng phí khu vực).

| Nhóm cấu hình | Gồm những gì |
|---|---|
| Máy chủ quán | port, tên miền nội bộ (mặc định `pos.local` nướng sẵn trong binary — cloud ghi đè được), khung giờ bảo trì/OTA |
| Nhân sự & quyền | danh sách nhân viên từng store, PIN (đồng bộ dạng băm), vai trò, quyền hủy món / giảm giá / mở két, ca làm việc |
| Chính sách vận hành | thời gian offline cho phép + các mức leo thang, ngưỡng cảnh báo, quy tắc busy-mode với app giao đồ ăn |
| Thiết bị & ngoại vi | license thiết bị (số lượng cho phép, danh sách đã ghép, thu hồi từ xa), máy in (vai trò hóa đơn/bếp, gán trạm), KDS (trạm, luật món → trạm), máy cà thẻ |
| Bán hàng & giao diện | menu, món, category, giá, khuyến mãi, layout màn order, sơ đồ bàn, phí phục vụ, VAT |
| Tích hợp | bật/tắt từng vendor theo store, mapping menu với Grab/ShopeeFood |

Bốn tình huống phải nói rõ để mô hình này chạy trơn:

1. **Đăng nhập khi mất mạng.** PIN/mật khẩu nhân viên đồng bộ xuống dạng băm → xác thực ngay tại quán, offline vẫn đăng nhập bình thường. Thu hồi một nhân viên có hiệu lực tức thì nếu store đang online, hoặc tại lần đồng bộ kế tiếp nếu đang offline.
2. **Máy in / KDS là đồ vật trong LAN — cloud không tự nhìn thấy chúng.** Quán tự *phát hiện* (quét cổng 9100 + USB) và báo lên cloud như **đề xuất**; admin đặt tên, gán vai trò/trạm trên dashboard; bản gán đó mới là cấu hình chính thức sync ngược xuống. Dữ liệu gốc sinh ở quán, quyền quyết vẫn ở cloud — vẫn đúng luật một nguồn chân lý.
3. **"Thời gian được phép offline" không được phá offline-first.** Đây là chính sách leo thang, không phải công tắc chặn: quá X giờ → banner cảnh báo trên màn POS + cảnh báo dashboard; quá Y → quản lý nhập PIN xác nhận mỗi ca. Tùy chọn chặn cứng có tồn tại nhưng **mặc định tắt** — chặn bán hàng là quyết định kinh doanh, không để hệ thống tự làm; giới hạn cứng tự nhiên vốn đã có: hết dải số hóa đơn được cấp là không phát hành hợp lệ được nữa.
4. **Cấu hình hỏng không được làm quán chết.** Mỗi gói cấu hình có version và được kiểm tra hợp lệ trước khi áp; không hợp lệ → từ chối, giữ bản tốt cuối cùng, báo lỗi lên dashboard — cùng triết lý với OTA tự kiểm tra và rollback. Dashboard hiển thị từng store đang chạy version cấu hình nào (nhìn một phát biết store nào chưa nhận bản mới), và mọi thay đổi đều có nhật ký: ai đổi, đổi gì, lúc nào.

---

## 8. Vendor & hóa đơn điện tử — tất cả là adapter

Core hệ thống chỉ gọi các **cổng chức năng cố định** (nhận đơn, đẩy menu, phát hành hóa đơn, thu tiền…); mỗi vendor là một **adapter** cắm vào cổng đó. Đổi hay thêm vendor = viết adapter mới + cấu hình, không đụng core. Mọi adapter dùng chung: hàng đợi riêng, thử lại, hộp thư lỗi, cầu dao ngắt (circuit breaker), bản giả lập để test, và **biểu đồ đo độ trễ riêng từng adapter** — để SLO "< 300ms phần của mình" có người canh.

| Nhóm | Cổng chức năng | Adapter | Phần chạy tại quán |
|---|---|---|---|
| Giao đồ ăn | nhận đơn, xác nhận, đẩy menu, báo hết món | Grab, ShopeeFood | — |
| ERP | kéo master data, đẩy doanh thu, đẩy tiêu hao | SAP | — |
| Thanh toán | tạo giao dịch, thu tiền, file đối soát | Payoo, ngân hàng | nối máy cà thẻ |
| Hóa đơn điện tử | cấp dải số, phát hành, tra cứu, đối soát | Viettel, VNPT, MISA… | phát hành offline |
| Giao vận | tạo/hủy chuyến, theo dõi tài xế (callback → sự kiện) | Ahamove, Grab Express… | hiển thị trạng thái trên POS |

**Hóa đơn là module quốc gia, không nằm trong core (định hướng core framework đa quốc gia).** Core trung lập: mọi bill thanh toán xong nhận **số biên nhận tăng liền mạch theo store** — bộ đếm nằm trong cùng transaction SQLite với bill, nên offline vẫn liền mạch tuyệt đối, không nhảy số trong vận hành bình thường. Toàn bộ nghĩa vụ pháp lý — số hóa đơn theo luật, ký số, gửi cơ quan thuế, quy tắc liên tục/đứt quãng vốn khác nhau tùy quốc gia — thuộc **module tài khóa cắm theo từng nước** qua port Fiscalization. Việt Nam (NĐ 123/2020, 70/2025, dải số cấp trước qua NCC, phát hành offline + đối soát như mô tả trong tài liệu này) là module đầu tiên, làm **sau** khi core POS chạy; quốc gia khác là module khác, core không đổi một dòng. Trong adapter chỉ có *giao thức của từng nhà cung cấp*; vòng đời pháp lý nằm ở module quốc gia; phần tại quán chỉ biết "dải số đã cấp + hàng đợi", không biết nhà cung cấp nào.

Quy tắc vận hành kèm theo: quán offline quá ngưỡng → tự bật chế độ bận / tạm ẩn món trên app giao đồ ăn để không vi phạm SLA xác nhận đơn.

---

## 9. Cập nhật phần mềm (OTA) — không bao giờ làm sập cả chuỗi

```
  Build + ký số (minisign, miễn phí) ──► Kho bản cài (MinIO)
                                              │
                                              ▼
                        VÒNG 1: quán thử nghiệm (1–3 quán)
                                              │ theo dõi 1–2 ngày
                              ổn ─────────────┼───────────── lỗi
                                              ▼                ▼
                        VÒNG 2: toàn bộ quán            DỪNG PHÁT HÀNH
                                                        (kill-switch từ cloud)

  Tại mỗi máy:  tải bản mới → kiểm chữ ký → cài vào giờ đóng cửa
                → tự kiểm tra (mở DB, in thử, bắt tay đồng bộ)
                → đạt thì giữ · lỗi thì TỰ quay về bản cũ + báo cloud
```

Với 50 quán, hai vòng là đủ (lên 1000 quán thì thêm vòng 25%). Cái giá duy nhất: một bản phát hành mất 1–2 ngày mới phủ hết — đổi lại không bao giờ có kịch bản "sáng ra cả chuỗi không bán được". UI của thiết bị client đi kèm bản cập nhật máy chủ quán, không có kênh update riêng.

---

## 10. Bảo mật & tuân thủ (gọn)

- **Kết nối:** mọi đường cloud↔quán là TLS do quán gọi ra; NATS dùng khóa riêng từng store — store chỉ nhìn thấy kênh của chính mình.
- **Danh tính máy:** mã kích hoạt một lần → credential dài hạn trong TPM/DPAPI (Windows) hoặc keyring (Linux); lease một-máy-một-quán (mục 6).
- **Trong quán:** thiết bị ghép bằng QR mới gọi được API; thao tác nhạy cảm (hủy món, giảm giá, mở két ngoài giao dịch) cần PIN theo vai trò; nhật ký thao tác không sửa được.
- **Máy:** mã hóa ổ đĩa; binary cập nhật luôn kiểm chữ ký trước khi chạy.
- **Ba dòng tuân thủ phải nhớ:** (1) hệ thống không làm CRM nhưng PII khách vẫn chảy qua — tên/SĐT/địa chỉ trong đơn Grab và thông tin người mua trên hóa đơn — nên VPS đặt tại Việt Nam và tự che/xóa các trường này sau N ngày (cấu hình được); (2) nghiệp vụ hóa đơn điện tử (thời hạn đẩy bù, điều chỉnh/hủy) chốt cùng nhà cung cấp + bộ phận thuế **trước khi viết code mô-đun này**; (3) telemetry chỉ là dữ liệu máy — không thiết kế bất kỳ tính năng giám sát hành vi nhân viên nào.

---

## 11. Con số: hiệu năng & dung lượng ở quy mô 50 store

Giả định nền (chỉnh được): quán trung bình 800 hóa đơn/ngày, mỗi hóa đơn ~8 sự kiện đồng bộ, giờ cao điểm gấp ~5 lần trung bình.

**Trong một quán** (máy chủ + 2–3 thiết bị trình duyệt): giờ cao điểm tạo 5–20 request/giây trong LAN; `pos_edge` phục vụ được > 5.000 request/giây ngay trên phần cứng phổ thông → dùng **dưới 1% công suất**, phản hồi micro–mili giây. SQLite ghi 1–3 giao dịch/giây so với năng lực > 10.000/giây. Số thiết bị đồng thời (CCU) trong quán là 3–30 — không đáng bàn ở mọi kịch bản.

**Trên VPS cloud (4 core Ampere / 24GB / 100GB) phục vụ 50 store:**

| Hạng mục | Nhu cầu @50 store | Năng lực box này | Mức dùng |
|---|---|---|---|
| Ghi sự kiện vào Postgres | ~4/giây trung bình · 20–40/giây đỉnh · burst ~100/giây khi store offline lâu dồn về | 2.000–5.000 insert/giây | ~1–2% |
| Kết nối đồng thời | 50 kết nối NATS dài hạn + vài chục phiên dashboard | NATS chịu hàng chục nghìn | không đáng kể |
| Query dashboard | vài chục/giây, trên bảng rollup | < 10ms mỗi query | mục tiêu <100ms: đạt |
| RAM | Postgres ~6 · NATS ~0.3 · pos_cloud ~0.3 · MinIO ~0.3 · metrics ~0.7 · OS ~1 → **~9GB** | 24GB | còn ~15GB làm cache |
| Đĩa | Postgres mọc ~150–250MB/ngày + MinIO 5–10GB + metrics | 100GB | đủ ~8–14 tháng |
| 4G mỗi quán | đồng bộ + số liệu ~5–15MB/ngày; OTA 30–60MB/lần | gói 4G phổ thông | thoải mái |

**Công thức định cỡ nhanh (kiểm chứng qua các kịch bản 300–1.000 quán, hình dạng cây Tenant/Brand/Store không ảnh hưởng — chỉ Σ quán và Σ bill/ngày quyết định):**

```
Đĩa Postgres:  GB/tháng ≈ bill/ngày toàn hệ × 0.15 ÷ 1000   (60k → 9GB · 500k → 75GB/tháng)
Ingest đỉnh:   sự kiện/giây ≈ bill/ngày ÷ 1.260              (trần 2.000–5.000/giây)
4G mỗi quán:   MB/ngày ≈ bill/ngày của quán × 0.003 + 2–5MB metrics
Backup Garage: GB ≈ số quán × 0.03–0.08
```

Hai ngoại lệ theo hình dạng cây: CCU dashboard ∝ số tenant (200 tenant ≈ 150 phiên đồng thời — vẫn <10ms nhờ rollup); fanout cấu hình ∝ số quán (1.000 message tức thì — không đáng kể với NATS). Bức tường tuyến tính duy nhất là ĐĨA — định cỡ trước bằng công thức trên; chi phí vận hành tuyến tính duy nhất là phí NCC hóa đơn điện tử (theo số bill). Chính sách giữ dữ liệu thô bao nhiêu năm: chốt với kế toán/thuế (hóa đơn thường phải tra cứu được ~10 năm → archive partition nén ra Garage là đường thoát đã có ở 14.10/14.11).

Ba ghi chú trung thực: (1) đĩa 100GB là ràng buộc **đầu tiên** sẽ chạm — không phải CPU/RAM; mua thêm dung lượng theo thời gian là đủ. (2) Backup Postgres bắt buộc đẩy ra khỏi box (mục 3.3). (3) Về CPU/RAM, box này gánh nổi cả **1000 store** (đỉnh ~400–800 sự kiện/giây vẫn dưới 30% năng lực ghi) — đường scale là thêm đĩa, thêm VPS thứ hai để có bản sao DB + NATS cluster, không phải đập kiến trúc.

---

## 12. Những thứ CỐ TÌNH không có (và lý do một dòng)

| Không dùng | Vì sao |
|---|---|
| Microservices trên cloud | Một binary nhanh hơn, nhẹ hơn, dễ vận hành hơn ở tải này; tách sau khi có bằng chứng cần |
| PgBouncer | Store không nối thẳng DB; chỉ app cloud với pool 20–50 kết nối → không có vấn đề để giải |
| ClickHouse + Debezium | Bảng rollup trong Postgres trả < 10ms; để dành khi thật sự cần phân tích ad-hoc lớn |
| Postgres tự failover (Patroni/etcd) | Cloud chết 15–30 phút không quán nào ngừng bán → backup + runbook khôi phục là đủ |
| App client riêng (Tauri/Electron) | Client chỉ là trình duyệt; UI đi cùng bản cập nhật máy chủ quán |
| Ship toàn bộ log về cloud | Chỉ đẩy lỗi + số liệu; log đầy đủ nằm tại quán, kéo trực tiếp qua NATS khi cần |
| Bản sao dữ liệu thứ 2 trong LAN | Đã có backup cloud liên tục; kịch bản nó che quá hiếm |
| Máy dự phòng chuyên dụng / chế độ khẩn cấp | Triết lý "máy chết thì thay" + khôi phục 5–10 phút thay thế trọn vẹn |
| Khóa cứng theo phần cứng (fingerprint) | Mâu thuẫn trực tiếp với thay máy nhanh → thay bằng lease |
| Firebase / Keycloak | Vài trăm tài khoản quản trị → tự viết auth gọn trong pos_cloud |
| Kênh giám sát nhân viên | Ngoài phạm vi và ngoài ranh giới cho phép |

---

## 13. Việc cần chốt trước khi code & lộ trình

Ba quyết định phụ thuộc bên ngoài, xếp vào tuần đầu pilot:

1. **Giao thức máy cà thẻ** — Payoo/ngân hàng có hỗ trợ giao thức TCP/serial không? (quyết định quán Linux nối máy cà thẻ trực tiếp được hay cần Windows).
2. **Chọn nhà cung cấp hóa đơn điện tử đầu tiên** — quyết định chi tiết adapter + nghiệp vụ offline, chốt cùng bộ phận thuế.
3. **ShopeeFood** — API trực tiếp hay qua đối tác trung gian.

Lộ trình:

| Giai đoạn | Quy mô | Mục tiêu |
|---|---|---|
| 1 — Pilot | 1–3 quán | Đủ hóa đơn điện tử + nghiệp vụ phục vụ tại bàn; chạy song song hệ thống cũ; chốt 3 quyết định trên |
| 2 — Tôi luyện | ~50 quán | OTA hai vòng, giám sát, quy trình thay máy, đối soát thanh toán chạy thật |
| 3 — Mở rộng | theo nhu cầu | Thêm đĩa/VPS thứ hai (bản sao DB, NATS cluster) rồi nhân bản lên hàng trăm–1000 quán |

Khối việc phát triển lớn nhất không nằm ở hạ tầng mà ở **nghiệp vụ phục vụ tại bàn**: sơ đồ bàn, tách–gộp–chuyển bàn, bắn món theo trình tự tới đúng trạm bếp, phí phục vụ + VAT, khuyến mãi, mở–chốt ca và kiểm két, phê duyệt của quản lý. Hạ tầng ở tài liệu này là nền để khối đó chạy nhanh và không mất dữ liệu.

---

## 14. Cơ chế bổ sung (chốt sau vòng rà soát cuối)

### 14.1 Realtime trong LAN & luật ghi trên order
`pos_edge` mở kênh đẩy (WebSocket/SSE) tới mọi client trong LAN: thêm món, bump bếp, hết món… hiện trên tất cả màn hình < 50ms, không ai phải bấm refresh. Thao tác trên order là **lệnh ghi thêm (append)** — hai thiết bị cùng thêm món vào một bàn thì hai lệnh tự hòa vào nhau; chỉ khi sửa/xóa *cùng một dòng* mới áp luật "lệnh sau thắng" + cả hai vào nhật ký. Đây là nền cho toàn bộ đặc tả nghiệp vụ đi kèm.

### 14.2 Hàng đợi in & an ninh cổng máy in
Mỗi lệnh in vào hàng đợi có trạng thái (chờ / đã in / lỗi), tự thử lại; máy in trạm hỏng → tự chuyển sang máy in dự phòng đã cấu hình, hoặc dồn cảnh báo đỏ lên KDS trạm đó (màn hình là fallback của giấy). An ninh: cổng 9100 của máy in mạng **không có xác thực** — ai vào được WiFi quán là gửi được lệnh in, kể cả lệnh kích mở két; vì vậy máy in nối két bắt buộc cắm USB vào máy chủ, hoặc toàn bộ thiết bị POS đi SSID/VLAN riêng tách khỏi WiFi khách.

### 14.3 Migration dữ liệu đi cùng OTA rollback
Bản cập nhật có nâng cấu trúc SQLite: trước khi migrate, copy nguyên file DB thành `.pre-update` (100–300MB, vài giây); rollback = trả lại cả binary lẫn file DB — tránh kịch bản binary cũ không đọc nổi DB đã bị bản mới sửa cấu trúc. Kỷ luật kèm theo: migration trong một bản phát hành chỉ được *thêm* (bảng/cột), không *xóa/đổi*, để hai phiên bản kề nhau luôn đọc chung được một DB.

### 14.4 Chìa khóa hệ thống

**Ai cấp ban đầu:** chính mình, không qua bên thứ ba nào (khác chứng chỉ HTTPS phải mua từ CA). Chạy `minisign -G` sinh ra cặp khóa: **khóa bí mật** để ký bản phát hành, **khóa công khai** nướng thẳng vào binary `pos_edge` — từ đó máy quán chỉ chịu cài bản nào mang chữ ký tương ứng. Mô hình tin cậy tự-cấp hoàn toàn hợp lệ vì mình kiểm soát cả hai đầu.

**Phân loại — hai nhóm, cách quản khác nhau:**

| Nhóm | Ví dụ | Quản ở đâu |
|---|---|---|
| Khóa vận hành hằng ngày | NATS credential từng store, token thiết bị, lease | **Trên cloud** — `pos_cloud` sinh, lưu Postgres, admin cấp/thu hồi qua dashboard |
| Khóa ký bản phát hành | minisign release key | **Ngoài cloud** — xem dưới |

Lý do khóa ký không để trên VPS: nó là khóa duy nhất mà nếu lộ, kẻ tấn công **không cần** xâm nhập quán nào — chỉ cần ký một bản `pos_edge` chứa mã độc, đẩy qua chính kênh OTA, và toàn fleet sẽ *tự nguyện* cài vì chữ ký hợp lệ. VPS phơi ra internet 24/7 trong khi khóa ký mỗi tháng chỉ dùng vài lần — để nó thường trực trên đó là đổi rủi ro liên tục lấy tiện lợi vài phút.

**Nơi cất, theo giai đoạn:**

| Cách | Ưu | Nhược | Dùng khi |
|---|---|---|---|
| USB + bản giấy niêm phong (2 bản, 2 nơi) | Offline tuyệt đối, 0 đồng | Phải cắm USB mỗi lần phát hành | **Hiện tại** |
| Password manager của team (Bitwarden self-host) | Tiện, có phân quyền + nhật ký | Vẫn là dữ liệu online | Khi 2–3 người cùng phát hành |
| Secret trong CI, chỉ tồn tại lúc chạy job | Ký tự động | Ai chiếm CI thì ký được | Khi build đã hoàn toàn tự động |

**Hai việc phải làm ngay từ ngày đầu (làm sau rất đau):**
1. **Sinh 2 cặp khóa, nướng CẢ HAI khóa công khai vào binary.** Khóa A ký thường ngày, khóa B dự phòng cất riêng nơi khác. Lộ hoặc mất A → chuyển sang B bằng một bản phát hành, không phải chạy ra từng máy.
2. **`pos_cloud` giữ danh sách khóa bị thu hồi**, edge kiểm tra trước khi cài. Đây chính là phần "quản trên cloud" của khóa ký: cloud quản *chính sách* (khóa nào còn hiệu lực), còn *vật chứa khóa bí mật* nằm ngoài cloud.

### 14.5 Backup: gồm gì, để đâu, và diễn tập restore

**Bốn thứ cần backup, mức quan trọng khác hẳn nhau:**

| Thứ | Là gì | Mất thì sao | Kích thước @50 store |
|---|---|---|---|
| Postgres cloud (kèm **WAL archiving liên tục** ra Garage → RPO phút thay vì theo nhịp dump) | Giao dịch mọi quán, cấu hình, master data | **Thảm họa** — mất dữ liệu toàn chuỗi | ~150–250MB/ngày → ~30–50GB/năm (nén) |
| Bản sao SQLite từng quán | Dữ liệu 90 ngày mỗi quán | Quán đó chết máy thì mất phần chưa sync | 30–80MB/quán → 5–10GB |
| Artifact OTA | Các bản `pos_edge` đã phát hành | Mất khả năng rollback nhanh (build lại được) | 20–50MB/bản |
| **Chìa khóa & config hạ tầng** | Khóa ký, NATS operator, compose/script | **Rất nghiêm trọng** — mất khóa ký = mất quyền cập nhật fleet | Vài KB |

Ba thứ đầu chảy vào object storage; thứ tư **tuyệt đối không** — nó phải sống sót cả khi mất trọn VPS (mục 14.4).

**Chọn object storage:**

| | MinIO | **Garage** | rclone thẳng ra đích ngoài |
|---|---|---|---|
| Bản chất | S3 server phổ biến nhất | S3 server viết bằng Rust, cực nhẹ | Không có server — chép file lên đích có sẵn |
| RAM | ~300MB–1GB | **~50–150MB** | ~0 (chạy theo cron) |
| S3 API | Đầy đủ nhất | Đủ cho Litestream (đã có người chạy thật) | Không có API → **không thay thế được** lớp 1 |
| Giấy phép | AGPL, đang siết dần bản community (đã cắt UI quản trị khỏi bản miễn phí) | AGPL, thuần OSS, cộng đồng nhỏ hơn | MIT |

Chọn: **Garage cho lớp 1** (nơi Litestream ghi vào — nhẹ hơn 5–10 lần, cùng là một binary, đúng tinh thần tài nguyên tối thiểu), **rclone cho lớp 2**. Lưu ý thực dụng: hai lựa chọn này **thay nhau trong một buổi** vì đều chỉ là "S3 endpoint" trong config — đừng tốn nhiều thời gian quyết; và cả hai sẽ **biến mất hoàn toàn** nếu tự viết WAL-shipping (Phụ lục D).

**Đích lớp 2** phải nằm ngoài VPS, tốt nhất ngoài luôn nhà cung cấp: snapshot của chính nhà cung cấp thì tiện nhưng tài khoản bị khóa là mất cả hai. Một VPS lưu trữ rẻ ở nhà cung cấp khác, hoặc một máy tại văn phòng chạy rclone kéo về, đều đủ.

**Dựng lại cloud từ các quán (thuộc tính phục hồi mạnh nhất của kiến trúc):** mỗi edge giữ 90 ngày sự kiện, nên mất dữ liệu cloud trong phạm vi đó là **phục hồi được bằng replay** — API nội bộ "reset cursor + replay từ ULID" ra lệnh cho các edge bơm lại. Chi phí gần bằng 0 vì dữ liệu đã có sẵn; chỉ cần lệnh.

**Diễn tập restore:** mỗi tuần một cron **tự lấy ngẫu nhiên backup của một quán, restore thử và kiểm tra mở được**; cùng lịch đó **restore Postgres cloud ra instance tạm và đối chiếu tổng doanh thu** — dữ liệu toàn chuỗi cũng phải được chứng minh khôi phục được, không chỉ dữ liệu từng quán — backup chưa từng restore thử thì chưa được tính là backup.

### 14.6 Đối soát tổng mỗi đêm
Chống lỗi đồng bộ *âm thầm* (một sự kiện rơi mất mà không ai biết): mỗi đêm quán tính số lượng + checksum của sự kiện N ngày gần nhất, cloud tính vế của mình; lệch → báo động đỏ kèm tự liệt kê ID thiếu để đẩy bù. Vài chục dòng code đổi lấy niềm tin tuyệt đối vào số liệu.

### 14.7 Cấu hình: đường lùi full snapshot
Quán offline lâu, lệch quá K version cấu hình (hoặc delta cũ đã bị dọn khỏi hàng đợi) → thay vì áp chuỗi delta, kéo nguyên bản chụp cấu hình đầy đủ — đúng cơ chế mà máy mới kích hoạt vẫn dùng, chỉ là dùng lại.

### 14.8 Sao chép WAL: lộ trình hai bước (Litestream → tự viết)

**Vì sao phần này khó:** SQLite ghi thay đổi vào file WAL rồi định kỳ *checkpoint* — dồn vào DB chính và **cắt bỏ** phần WAL đã dồn. Công cụ backup phải chép các khúc WAL lên cloud **trước khi** chúng bị cắt; hụt một khúc là chuỗi khôi phục đứt và bản backup vô dụng. Đây cũng là chỗ Litestream trên Windows ít được kiểm chứng (Windows khóa file khác Linux).

| | Dùng Litestream | Tự viết trong `pos_edge` |
|---|---|---|
| Thời gian | Vài ngày cấu hình + kiểm thử | 1–2 tuần code + 1–2 tuần thử thật |
| Độ tin cậy | Đã chạy production nhiều nơi trên Linux; **Windows ít kiểm chứng** | Bằng đúng chất lượng test của mình — nhưng test đúng kịch bản của mình |
| Vận hành | Thêm 1 tiến trình phải giám sát, cấu hình, cập nhật riêng | Một binary duy nhất; backup và app cùng vòng đời |
| Khi hỏng | Chờ upstream vá hoặc tự fork | Tự sửa ngay |
| Tích hợp | Lỗi nằm ở log riêng, phải bắc cầu mới lên dashboard | Trạng thái backup vào thẳng heartbeat → heatmap fleet |
| RPO | Vài giây (chu kỳ quét ~1s) | **< 1 giây** (đẩy ngay khi ghi) |
| Kèm theo | Cần một S3 server (Garage/MinIO) | **Xóa luôn Garage/MinIO** — đẩy thẳng qua kênh đã có |

**Lộ trình:** pilot dùng Litestream ngay (nhanh, không tốn effort lúc đang cần chạy nghiệp vụ), nhưng coi là **hạng mục kiểm chứng số 1** với bài test ác: rút điện giữa lúc bán, kill process, mạng chập chờn, chạy liên tục 2 tuần, rồi restore và đối chiếu từng bill. Qua thì giữ; không qua thì tự viết — và khi đó **đã có sẵn bộ test** để kiểm chứng bản tự viết. Đó mới là giá trị thật của việc đi hai bước.

**Thứ tự các lớp bảo vệ (quan trọng để không hoảng nếu Litestream chưa hoàn hảo):** outbox là **lớp một** — thứ không thể mất là sự kiện chưa sync, mà chúng đã được đẩy lên cloud trong vài giây. WAL-shipping là **lớp hai**, phục vụ khôi phục nhanh trạng thái làm việc (bàn đang mở, ca đang chạy) thay vì dựng lại từ cloud. Lỗi ở lớp hai làm chậm khôi phục, không làm mất doanh thu.

### 14.9 Gác lại có chủ đích
Giám sát nhịp tim từ **bên ngoài** VPS (VPS chết thì hệ cảnh báo trên nó cũng im lặng theo): chưa cần ở giai đoạn này; bật khi hệ thống trở thành nguồn số liệu vận hành chính — khi đó chỉ là một cron/dịch vụ ping miễn phí từ ngoài, nhắn Zalo/Telegram nếu cloud mất tích quá 2 phút.

---

## 14.10 Bốn cơ chế vận hành còn thiếu (bổ sung sau vòng rà soát framework)

1. **`/health` và `/ready`** trên `pos_cloud` (~20 dòng): `/health` = process sống; `/ready` = DB + NATS + storage đều thông và không đang migration. Deploy script dùng `/ready` để biết đã lên xong; giám sát dùng `/health`. Không có hai endpoint này thì không phân biệt được "cloud sống nhưng DB đứt".
2. **Luật kết nối bền ở edge:** mọi lệnh gọi lên cloud dùng retry + backoff lũy tiến + jitter, và **tuyệt đối không chặn đường bán hàng** khi cloud/DB không phản hồi — sự kiện nằm yên trong outbox. Đây là lý do kiến trúc không cần HA tự động ở cloud; thiếu nó thì người ta sẽ "sửa sai" bằng cách thêm proxy/HA vô ích.
3. **Job dọn dữ liệu (retention):** cron trên cloud xóa/che PII theo hạn **cấu hình** (framework không cố định con số — Đặc tả v2 mục 25; PII lưu tách khỏi event log qua `subject_id` nên ẩn danh không phải viết lại lịch sử) (nghĩa vụ pháp lý ở mục 10 hiện đã ghi nhưng chưa có ai thực thi) và archive partition cũ khi đĩa tới ngưỡng — trả lời luôn câu hỏi "đầy 100GB sau 8–14 tháng thì sao".
4. **Trang "Trạng thái hệ thống"** cho admin: sức khỏe *cloud* (sync lag, độ sâu hàng đợi, đĩa còn, thời điểm backup gần nhất, số store lệch version cấu hình) — bổ sung cho heatmap fleet vốn chỉ nhìn phía quán.

## 14.10b Spec phần cứng tối thiểu (cho pilot & hướng dẫn mua sắm)

| Thiết bị | Yêu cầu tối thiểu |
|---|---|
| Máy chủ quán | x86-64, 4GB RAM, **SSD/NVMe (cấm thẻ nhớ, cấm HDD)**, Windows 10+ hoặc Linux, UPS nhỏ khuyến nghị |
| Tablet order | Android 10+ / iPadOS 15+, màn ≥ 8", WiFi 5 |
| Màn KDS | Bất kỳ màn ≥ 21" + máy tính nhỏ hoặc tablet ≥ 10", đặt cách tầm nhìn ≤ 2m |
| Máy in | ESC/POS, LAN (VLAN riêng) hoặc USB; máy in nối két **bắt buộc USB** (an ninh 14.2) |
| Mạng | Router hỗ trợ DHCP reservation; SSID/VLAN tách khỏi WiFi khách; 4G dự phòng tùy chọn |
| VPS cloud | 4 core, 24GB RAM, **NVMe cục bộ** (14.11), 100GB+ tùy quy mô (công thức mục 11) |

## 14.11 Tài nguyên phải có giới hạn — checklist chống tràn (cấu hình bắt buộc của bootstrap/compose)

| Véc-tơ | Chặn bằng |
|---|---|
| JetStream stream phình khi store offline lâu | `max_age` + `max_bytes` từng stream, cảnh báo khi chạm 80% |
| Log container Docker (mặc định không giới hạn) | `log-opt max-size=10m, max-file=3` trong compose |
| Image Docker cũ dồn lại sau mỗi deploy | `docker image prune` trong bước deploy |
| Thế hệ backup Litestream trên Garage | Retention giữ N bản snapshot + WAL tương ứng |
| WAL SQLite nở khi replication kẹt checkpoint | Watchdog đo kích thước WAL, vượt ngưỡng → cảnh báo đỏ |
| Postgres tăng trưởng | Job retention (14.10) + archive partition |
| RAM in-process | Luật code: channel/cache bounded (Kế hoạch GitHub mục 11) |
| Kho metrics | Luật cardinality: label chỉ theo store/adapter, cấm label theo đơn |

Kèm một yêu cầu phần cứng tường minh: **VPS phải dùng NVMe cục bộ** — điểm nghẽn đầu tiên của cả hệ là fsync (JetStream + Postgres); đĩa mạng (block storage) fsync 5–20ms sẽ hạ trần ghi xuống nhiều lần.

## 14.12 Ảnh menu — luồng dữ liệu lớn nhất tại quán

Ảnh món phải hiện được khi offline → là một phần của đồng bộ cấu hình: upload trên dashboard → nén server-side về WebP **hai kích thước**: thumbnail ≤30KB (danh sách, dùng cho QR ordering) và ảnh chi tiết ≤150KB (≤800px) → lưu Garage → sync xuống edge theo delta như mọi cấu hình khác (~20MB cho menu 200 món). Luật cứng: không bao giờ đẩy ảnh gốc từ điện thoại (8MB × 200 món × 1000 quán) xuống fleet.

---

## 15. Cấu trúc Framework & Core

### 15.1 Nguyên tắc: sở hữu RANH GIỚI, không phải sở hữu implementation

Đây là câu trả lời cho "framework hoàn thiện, không phụ thuộc". Domain không bao giờ gọi thẳng NATS, Postgres, S3 hay máy in — nó chỉ gọi **port** (trait) do mình định nghĩa. Mọi thư viện bên ngoài chỉ là *một* implementation của một port. Ngày muốn thay: viết implementation thứ hai, đổi một dòng lắp ráp — **không sửa một dòng domain nào**. Chi phí gần bằng 0 nếu làm từ commit đầu tiên, đắt gấp nhiều lần nếu làm sau.

### 15.2 Cây workspace

```
pos-framework/                    (Cargo workspace, monorepo)
├── crates/
│   ├── pos-core/                 DOMAIN THUẦN — luật nghiệp vụ, state machine,
│   │                             tính tiền, ca, order. KHÔNG import tokio/sqlx/
│   │                             axum/mạng/filesystem. Test chạy bằng mili giây.
│   ├── pos-ports/                Định nghĩa trait (cổng) — cũng thuần
│   ├── pos-proto/                Kiểu dữ liệu + sự kiện dùng chung cloud↔edge,
│   │                             kèm PROTOCOL_VERSION
│   ├── adapters/
│   │   ├── store-sqlite/         EventStore, ConfigStore tại quán
│   │   ├── store-postgres/       phía cloud
│   │   ├── link-nats/            MessageLink (thay được)
│   │   ├── blob-s3/              BlobStore (biến mất khi tự viết WAL-shipping)
│   │   ├── printer-escpos/       PrinterDriver
│   │   ├── payment-payoo/        PaymentTerminal
│   │   ├── vendor-grab/          DeliveryVendor
│   │   ├── erp-sap/              ErpSink
│   │   └── fiscal-vn/            Fiscalization — MODULE QUỐC GIA đầu tiên
│   ├── pos-edge/                 binary: lắp adapter vào core
│   ├── pos-cloud/                binary: lắp adapter vào core
│   └── pos-simulator/            fleet simulator (N quán ảo)
├── ui/                           SolidJS
└── docs/                         kiến trúc, nghiệp vụ, ADR
```

### 15.3 Danh sách port

| Port | Trách nhiệm | Implementation hiện tại | Kế hoạch |
|---|---|---|---|
| `EventStore` | Ghi/đọc sự kiện, outbox | SQLite / Postgres | giữ |
| `ConfigStore` | Bản chụp + delta cấu hình | SQLite / Postgres | giữ |
| `MessageLink` | Kênh cloud↔quán, pub/sub bền | NATS JetStream | giữ (xem Phụ lục D) |
| `BlobStore` | Lưu khối lớn (backup, artifact) | Garage/MinIO (S3) | **tự viết → xóa hẳn port này** |
| `MetricsSink` | Đẩy số đo | VictoriaMetrics | giữ |
| `Signer` / `KeyVault` | Ký & kiểm chữ ký, cất khóa | minisign + keyring OS | tự viết phần verify |
| `ClockSource` | Giờ + phát hiện lệch | SNTP | tự viết |
| `IdGenerator` | ULID | crate | tự viết |
| `PrinterDriver` | ESC/POS, hàng đợi in | tự viết ngay từ đầu | — |
| `PaymentTerminal` | Máy cà thẻ (FFI / TCP) | tự viết ngay từ đầu | — |
| `Fiscalization` | Nghĩa vụ hóa đơn theo quốc gia | `fiscal-vn` | thêm nước = thêm crate |
| `DeliveryVendor` / `ErpSink` | Vendor bên ngoài | adapter riêng | thêm vendor = thêm crate |

### 15.4 Luật phụ thuộc (CI bắt buộc thi hành)

1. `pos-core` và `pos-ports` chỉ được import: `std`, `serde`, và crate thuần tính toán. **CI có test kiểm tra danh sách dependency của hai crate này** — thêm crate hạ tầng vào là fail build.
2. Adapter được phụ thuộc core/ports; **core không bao giờ phụ thuộc adapter**.
3. Binary (`pos-edge`, `pos-cloud`) là nơi *duy nhất* biết adapter nào được lắp.

Hệ quả trực tiếp: test nghiệp vụ (toàn bộ đặc tả POS) chạy **không cần DB, không cần mạng, không cần máy in** — bằng fake in-memory, vài giây cho toàn bộ suite.

### 15.5 State machine tối thiểu trong core (mảnh cuối của chữ "Core")

`pos-core` phải chứa bảng **trạng thái × sự kiện → trạng thái mới, kèm bất biến** cho Order, Bill, Shift (và Table) — dạng dữ liệu/mã, không phải văn xuôi. Ví dụ bất biến: bill đã settled thì không nhận thêm dòng món · dòng đã fired chỉ hủy được kèm lý do + quyền quản lý · shift đã đóng thì không giao dịch nào gắn vào được nữa · tổng thanh toán luôn bằng tổng bill. Ba lý do bắt buộc có: người viết adapter dựa vào nó, test property-based bám vào nó để bắt bug nghiệp vụ mắt người không thấy, và AI contributor không thể tự suy diễn sai. Đặc tả nghiệp vụ (tài liệu đi kèm) là bản mô tả cho người; bảng này là bản mô tả cho máy — hai thứ phải khớp và CI kiểm chứng bằng test.

### 15.6 Contract test cho mỗi port

Mỗi port đi kèm **một bộ test dùng chung** mà *mọi* implementation phải vượt qua (ví dụ `EventStore`: ghi rồi đọc lại đúng thứ tự, idempotent theo ULID, sống sót khi mô phỏng crash giữa transaction). Đây là thứ biến "thay được implementation" từ lời hứa thành sự thật kiểm chứng được — và là chuẩn mực phân biệt một framework nghiêm túc với một ứng dụng có nhiều file.

---

## 16. Đa quốc gia — kiến trúc cell

**Lời hứa của framework, một câu:** quốc gia là *tham số lúc deploy*, không phải *giả định trong code*. "Nước nhà" là vai trò gán cho cell đầu tiên — với chúng tôi là Việt Nam; với người fork ở Nhật, cell đầu tiên của họ chính là Nhật, domain trần là của họ, locale pack đổ theo câu trả lời trong wizard first-boot ("Quốc gia?" — hỏi như hỏi múi giờ). Mọi thứ mang màu quốc gia (fiscal, vendor giao đồ ăn, cổng thanh toán) đều là crate plugin; core không chứa một giả định quốc gia nào. Ba tầng của bài toán và mức cần thiết: (A) core trung lập quốc gia — **bắt buộc, làm từ commit đầu, chi phí ≈ kỷ luật**; (B) một tổ chức vận hành nhiều nước cùng lúc — quyết định trên giấy dưới đây, kích hoạt khi có ≥ 2 quốc gia, hôm nay ~20 dòng code (parse Host nhận nhãn nước); (C) trải nghiệm một domain — redirect, làm cùng lúc với B.

### 16.1 Latency không phải vấn đề (và vì sao)

Cloud không nằm trên đường đi của giao dịch, nên khoảng cách địa lý gần như vô hại với vận hành quán:

| Luồng | Qua cloud? | Ảnh hưởng khi RTT 80–200ms (VN↔JP/EU) |
|---|---|---|
| Order, in, thanh toán, hóa đơn tại quán | Không — 100% LAN | Zero |
| Sync sự kiện | Có, bất đồng bộ | Lag 1–3s → 1.5–3.5s, không cảm nhận được |
| Hot-reload cấu hình | Có | <1s → ~1.2s |
| Dashboard admin | Có, tương tác | Chỗ duy nhất cảm nhận được (+200ms/thao tác ở EU) |

### 16.2 Lý do thật để tách: pháp lý, không phải mạng

PDPD (VN), PIPL (TQ), GDPR (EU)… ràng buộc dữ liệu cá nhân cư dân từng nước. Một database chung chứa PII đa quốc gia là bài toán tuân thủ không đáng giải.

**Thiết kế cho tầng B — mỗi quốc gia một CELL** — một bản deploy độc lập trọn vẹn (VPS + Postgres + NATS + Garage + domain riêng, ví dụ `vn.pos.company`, `jp.pos.company`), cắm module tài khóa nước đó (`fiscal-vn`, `fiscal-jp`…). Quán nối cell nước mình. Dựng cell mới = thêm một GitHub Environment + một VPS + chạy workflow deploy (Kế hoạch GitHub mục 17) — cơ chế fork-and-deploy chính là cỗ máy nhân bản cell.

Tính chất: các cell **không biết nhau** — một cell sự cố không ảnh hưởng nước khác; dữ liệu không bao giờ vượt biên giới; nâng cấp chạy theo từng Environment, canary rings áp dụng trong từng cell.

### 16.3 Nhìn toàn cầu (giai đoạn sau)

Hai mức: (a) chưa có tầng global — đăng nhập từng cell, đủ cho 2–3 nước đầu; (b) **global rollup** — mỗi cell đẩy số liệu tổng hợp *không chứa PII* (doanh thu, số đơn, health) về một dashboard toàn cầu nhỏ; chỉ aggregate vượt biên giới nên sạch pháp lý. Làm (a) ngay, thiết kế schema sự kiện rollup để (b) bật được mà không sửa cell. Không làm global control plane tập trung (một DB thế giới) ở mọi quy mô trong tầm nhìn.

### 16.4 Đặt tên: tên phẳng theo tenant, DNS định tuyến — không nhãn nước, không proxy

**Luật một dòng: đứng trước các cell chỉ được REDIRECT, không bao giờ PROXY.** Phương án path (`domain.com/jp`) bị bác vì là proxy: mọi request nước khác phải quá cảnh và bị giải mã tại gateway một nước (phá lý do pháp lý của cell), cộng 100–160ms vòng vèo, gateway thành SPOF toàn cầu, cookie chung hostname phá cách ly phiên. Kiểu Google (một tên toàn cầu) thực chất là GeoDNS + anycast + backbone riêng — tách quốc gia ở tầng mạng vô hình; không sao chép được với 2 VPS, và geo-routing đưa người dùng tới server *gần nhất* chứ không phải nơi *dữ liệu của họ* sống.

**Chọn: tên phẳng theo tenant (mô hình Slack/Zendesk).** Mỗi tenant sống ở đúng một cell → record DNS của tenant trỏ thẳng cell đó, không nhãn nước nào lộ ra:

```
*.domain.com          → wildcard mặc định → cell nước gốc (VN)
pizza4ps.domain.com   → (dùng wildcard)   → cell Việt Nam
sushitaro.domain.com  → record riêng      → cell Nhật (tạo qua CF API lúc lập tenant)
```

Cơ chế: (a) tenant ở nước gốc dùng wildcard, không tốn record; tenant nước khác được cell của nó tự tạo một record qua Cloudflare API (`CF_DNS_API_TOKEN` sẵn có) — số record = số tenant ngoài nước gốc; (b) **chống trùng slug toàn cầu miễn phí**: tạo record chính là phép kiểm tra — record tồn tại nghĩa là slug có chủ; Cloudflare DNS là sổ cái duy nhất, không cần database chung giữa các cell; (c) cert: mỗi cell tự giữ một wildcard `*.domain.com` qua DNS-01 (Let's Encrypt cho phép nhiều bản cùng wildcard; khi vượt ~5 cell, giãn lịch gia hạn để tránh trần duplicate-certificate); (d) webhook vendor cũng phẳng: `pizza4ps.domain.com/webhooks/grab`. Nhãn nước chỉ tồn tại ở endpoint vận hành cho đội ops: `admin.vn.domain.com`, `admin.jp.domain.com` — người dùng không bao giờ thấy. `domain.com` gốc được phép làm trang định hướng (redirect tĩnh, không PII).

**Luồng bật quốc gia mới:** (1) tạo GitHub Environment + secrets VPS/GKE nước đó; (2) Run workflow → cell dựng ~10 phút; (3) thêm record `admin.cc.domain.com` → IP cell; (4) `/setup` cell mới → siêu quản trị (cùng người với cell khác cũng được — hai tài khoản, hai cell); (5) tạo tenant → record tenant tự sinh → export → quán kích hoạt. Trung thực về chi phí: công tắc ~15 phút, nhưng *vào một quốc gia* = viết `fiscal-cc` + locale pack + adapter vendor bản địa (tuần công việc) — toàn bộ là crate adapter, core không đổi một dòng.

### 16.5 Bốn việc đa quốc gia kéo theo (đưa vào backlog ngay)

1. **i18n từ commit đầu:** mọi chuỗi UI qua file dịch theo locale, không hardcode; ngôn ngữ là cấu hình theo store/thiết bị. Hệ thống đầy đủ: Đặc tả v2 mục 21 — hai tầng chuỗi (UI framework / nội dung tenant), tiếng Anh là gốc bắt buộc, admin quản bản dịch trên cloud, ICU MessageFormat, in bitmap cho ký tự ngoài bảng mã máy in. Làm sau là dự án khảo cổ.
2. **Locale pack là nhóm cấu hình hạng nhất** (bổ sung bảng 7.1): tiền tệ, múi giờ, format ngày/số, template biên nhận theo nước.
3. **Rollup "theo ngày" tính theo múi giờ của STORE**, không phải giờ server — nguồn bug kinh điển làm lệch doanh thu ngày.
4. Khi số cell tăng, chuyển giao image từ save/load qua SSH sang registry (ADR riêng — Kế hoạch GitHub 17.4).

---

## 17. Bề mặt tích hợp mở — Public API & Webhooks

### 17.1 Hai bề mặt, một nguồn sự kiện
Nguồn duy nhất là event log sẵn có (sự kiện bán hàng từ quán, trễ 1–3s + sự kiện cấu hình, tức thì). Hai bề mặt cắm vào: **Webhook Dispatcher** (push) và **Cursor Feed** `GET /v1/events?page_size=&page_token=<ulid>` (pull — tự quyết nhịp, replay từ bất kỳ điểm nào; khuyến nghị cho BI/đồng bộ kho dữ liệu). Public REST API tại `api.domain.com`, version `/v1`, kỷ luật chỉ-thêm, OpenAPI tự sinh.

### 17.2 Mô hình cursor-trên-log — không tồn tại queue để sập
Dispatcher không có hàng đợi riêng: mỗi endpoint là một **cursor trên event log** (Postgres, bounded theo retention). Endpoint chậm/chết = cursor tụt lại — không gì phình, không backpressure vào đường ingest. Đổi lại chấp nhận trễ quét 100–200ms → end-to-end quán → server tenant ~1.5–3.5s (chuẩn ngành). Cố tình KHÔNG bắn webhook từ đường ingest nóng — không buộc sức khỏe endpoint của tenant vào đường sống của hệ thống.

### 17.3 Chuẩn giao hàng webhook
HMAC-SHA256 secret riêng từng endpoint + timestamp ±5 phút + ULID idempotency · at-least-once, **không cam kết thứ tự** (sự kiện lỗi retry độc lập, chống head-of-line; bên nhận sắp bằng ULID) · backoff ~8 lần/24h → dead-letter + nút redeliver · cách ly per-endpoint (pool ≤4 concurrent, timeout 5–10s, circuit breaker, tự vô hiệu sau 24h lỗi + cảnh báo) · trần toàn cục in-flight (RAM bounded, 14.11) · drain sau downtime có trần tốc độ + nút "bỏ qua backlog" (backfill bằng feed). **Chống SSRF:** HTTPS-only, chặn dải IP private/loopback/link-local, resolve rồi ghim IP (chống DNS rebinding), không theo redirect.

### 17.4 Public API v1
- **Auth:** API key theo tenant, có scope (`read:orders`, `write:menu`, `orders:create`…), thu hẹp được xuống brand/store; băm khi lưu, thu hồi tức thì, có last-used.
- **Đọc:** đơn/bill/báo cáo (từ rollup, <10ms), menu + trạng thái hết món.
- **Ghi menu/86:** chính là ghi cây cấu hình → tái dùng trọn hot-reload — bên thứ ba sửa món, mọi quán của brand thấy < 1 giây.
- **Tạo đơn từ kênh ngoài** `POST /v1/orders` (bắt buộc idempotency key = mã đơn của kênh): tái dùng port `OrderIn` — website/app riêng của tenant và kênh bán mới tự tích hợp theo chuẩn công khai; ông lớn (Grab Food…) vẫn theo hướng adapter. Quán offline → trả busy theo đúng luật vendor sẵn có.
- **Giới hạn theo tenant trong cây cấu hình:** rate limit, số endpoint, ngày giữ log giao hàng. **PII trong payload:** mặc định loại; bật per-endpoint bằng config kèm nhắc DPA/PDPD ngay trên UI.

### 17.5 Nhóm vendor thứ 5 — Giao vận
Port `ShippingDispatch`: `CreateDelivery` / `Cancel` / `Track` + callback trạng thái (đã nhận đơn, đang giao, hoàn tất) → thành sự kiện → hiện trên POS + đẩy webhook. Mỗi hãng (Ahamove, Grab Express…) một crate adapter như mọi vendor.

### 17.6 Con số & ngưỡng mở rộng
Worst-case @300 quán (mọi tenant sub mọi sự kiện × 5 endpoint): ~28 POST/giây trung bình, ~150/giây đỉnh — không đáng kể; HMAC ~micro giây. Ngưỡng để mở tiếp: batch nhiều sự kiện/POST khi một tenant vượt ~500 sự kiện/giây; OAuth2 + developer portal khi mở marketplace đại trà. Màn hình quản trị: mục "Developers" theo tenant (danh mục màn hình dashboard ⑦).

---

## Phụ lục E — Công nghệ đã xem xét và LOẠI (kèm ngưỡng đổi ý)

**Luật kết nạp thành phần hạ tầng mới** — muốn thêm bất cứ thứ gì vào stack phải trả lời được cả 4 câu: (1) nó *thay thế* cái gì đang có? (2) con số nào chứng minh cần? (3) thêm bao nhiêu RAM/daemon/điểm hỏng? (4) nếu sai thì bỏ ra được không? Không đủ 4 câu → không thêm.

| Công nghệ | Vì sao KHÔNG cần ở kiến trúc này | Ngưỡng đổi ý |
|---|---|---|
| **Valkey / Redis** | Cả 5 việc của nó đã có chỗ khác lo: cache → bảng rollup + page cache (query đã <10ms) · session → cookie có chữ ký (vài trăm admin) · queue → JetStream · pub/sub → NATS + WebSocket · rate limit → bộ đếm in-process (monolith). Sở trường của Redis là cache read-heavy nhiều instance — trong khi tải cloud là **write-heavy nhẹ** (~40 event/giây đỉnh). Thêm vào = +1 daemon, ~200MB RAM, và một chỗ dữ liệu có thể lệch với DB (cache invalidation) | Cloud chạy nhiều instance **và** Postgres CPU cao vì đọc |
| **HAProxy / Nginx** | Chỉ có 1 backend để cân bằng → thành cách viết `proxy_pass` phức tạp. Caddy đang lo phần thật cần (TLS tự động) và theo lộ trình bị hấp thụ vào `pos_cloud` — hướng đi là *bớt* proxy | Cần load-balance nhiều instance (khi đó Caddy cũng làm được; HAProxy chỉ hơn ở hàng chục nghìn kết nối/giây) |
| **Kubernetes / service mesh** | Toàn hệ thống là 4 container trên 1 VPS; K8s là hệ điều hành phân tán cho hàng trăm service | Không, ở mọi quy mô trong tầm nhìn (GKE là làn tùy chọn cho ai đã có GCP) |
| **Kafka** | JetStream đủ và nhẹ hơn nhiều bậc | Không |
| **ELK / Loki full-log** | Đã chọn: chỉ ship lỗi + metrics, log đầy đủ nằm tại quán, kéo trực tiếp qua NATS | Không |
| **Patroni / etcd (auto-failover)** | Cloud chết 15–30 phút không quán nào ngừng bán → backup + runbook đủ | Khi cloud trở thành đường đi của giao dịch (không nằm trong thiết kế) |
| **ClickHouse + Debezium** | Bảng rollup trả <10ms | Cần phân tích ad-hoc lớn trên dữ liệu thô nhiều năm |
| **SolidJS → Leptos (Rust→WASM)** | Leptos cho toàn stack một ngôn ngữ Rust (hợp mục tiêu all-Rust) + bundle WASM nhỏ; SolidJS trưởng thành hơn về hệ component, tooling, số dev biết. **Đã chốt: giữ SolidJS** (quyết định xác nhận, không còn để mở) — độ chín hệ sinh thái và tooling thắng lợi thế thuần-Rust | Nếu ưu tiên thuần-Rust toàn stack: đổi sang Leptos — UI nằm sau ranh giới rõ, không ảnh hưởng core/cloud |
| **Bỏ NATS → Postgres-as-queue + SSE** | Khả thi ở quy mô nhỏ (SKIP LOCKED + SSE đều outbound, giải CGNAT y hệt), bớt 1 daemon ~0.3GB — nhưng phải tự viết ~500–1000 dòng plumbing ack/redelivery/dedupe/push, đúng loại code mà bug thành "mất đơn không rõ nguyên nhân"; NATS cho sẵn ngữ nghĩa đó | Nếu muốn tối giản daemon tuyệt đối: port `MessageLink` cho phép đổi implementation không đụng domain |

---

## Phụ lục A — Tech stack (toàn bộ mã nguồn mở)

| Thành phần | Lựa chọn | Ghi chú |
|---|---|---|
| Máy chủ quán | Rust (tokio, Axum) — `pos_edge` | 1 binary, build Windows + Linux |
| DB tại quán | SQLite (WAL) | Giữ 90 ngày; synchronous=NORMAL |
| Backup quán → cloud | Litestream | Đẩy liên tục lên MinIO, trễ vài giây |
| Giao diện | SolidJS + Tailwind, nhúng rust-embed | Client = trình duyệt, không app riêng |
| Cloud | Rust — `pos_cloud` (modular monolith) | 1 binary: API, hub vendor, fleet, auth |
| Hàng đợi | NATS JetStream | 1 node trên VPS đơn; outbound-only từ quán |
| DB trung tâm | PostgreSQL | Partition theo store + RLS + JSONB + bảng rollup |
| Kho file | MinIO (hoặc Garage) | Backup store, bản OTA, file xuất |
| Giám sát | VictoriaMetrics + Grafana | Lỗi + số liệu; log giữ tại quán |
| Ký số bản cài | minisign (ed25519) | Khóa tự quản, 0 đồng |
| ID giao dịch | ULID | Sinh tại quán, chống trùng khi đồng bộ |

## Phụ lục B — Lịch sử thay đổi

- **v1 → v2:** thêm lớp hóa đơn điện tử; webhook có hàng đợi bền + DLQ; bỏ "1000 database riêng" → Postgres partition chung; OTA từ "thay file 2h sáng" → ký số + vòng phát hành + tự rollback; thêm backup liên tục từ quán; bảo mật LAN (ghép thiết bị, PIN vai trò); bổ sung khối nghiệp vụ tại bàn; các mục tuân thủ.
- **v2 → v2.1:** hóa đơn điện tử gộp vào hub vendor (ports & adapters thống nhất); đa nền tảng Windows/Linux; máy chủ quán tách khỏi màn hình thu ngân; triết lý "máy chết thì thay" — bỏ máy dự phòng bắt buộc; bỏ khóa cứng phần cứng → lease.
- **v2.1 → v2.2:** Windows trở lại hạng nhất (license ngoài phạm vi chi phí); cloud gộp thành một binary (modular monolith); cắt PgBouncer, Tauri, Patroni, ClickHouse/Debezium, ship-toàn-bộ-log, bản sao LAN thứ hai, Firebase/Keycloak; thêm MinIO tự host, auth tự viết, bảng rollup, kéo log trực tiếp qua NATS, luồng cài "chạy file + gõ mã", quán chỉ giữ 90 ngày dữ liệu; làm rõ lease (không hết hạn khi offline, máy mới nhận dải số hóa đơn mới); bổ sung bảng dung lượng thực tế @50 store trên VPS 4 core/24GB/100GB.
- **v2.2 → v2.3:** cấu hình tập trung 100% trên cloud — cây cấu hình Tenant → Brand → Store → Thiết bị phủ toàn bộ (port/domain máy chủ quán, nhân sự & PIN, phân quyền, thời gian offline cho phép, license thiết bị, máy in, KDS, menu/category/layout, tích hợp); quán không còn file chỉnh tay; file cài đổi thành bootstrapper luôn kéo bản mới nhất từ cloud; đăng nhập offline bằng PIN băm đồng bộ sẵn; luồng "quán phát hiện → admin duyệt" cho máy in/KDS; chính sách thời gian offline leo thang, mặc định không chặn bán hàng.
- **v2.3 → v2.4:** core POS trung lập quốc gia — số biên nhận liền mạch theo store nằm trong core, hóa đơn pháp lý trở thành module tài khóa cắm theo từng nước (VN là module đầu tiên, làm sau core); thêm mục 14 với 8 cơ chế chốt sau rà soát (realtime LAN + luật append trên order, hàng đợi in + an ninh cổng 9100, migration đi cùng rollback, quản lý chìa khóa, backup của backup + diễn tập restore, đối soát tổng mỗi đêm, full-snapshot cấu hình, kiểm chứng Litestream/Windows) và 1 mục gác lại có chủ đích (giám sát từ bên ngoài); ghép thiết bị bằng QR-chứa-link + đường lùi nhập tay IP:port + mã ghép 6 số; thêm Phụ lục C (vì sao chọn từng công nghệ); phát hành kèm tài liệu Đặc tả nghiệp vụ POS v1.
- **v2.4 → v2.5:** thêm mục 15 (cấu trúc Framework & Core: workspace, danh sách port, luật phụ thuộc CI thi hành, contract test cho mỗi port) — nền tảng cho mục tiêu "framework hoàn thiện, không phụ thuộc"; mở rộng 14.4 (ai cấp khóa ban đầu, mô hình 2 khóa + danh sách thu hồi trên cloud, nơi cất theo giai đoạn), 14.5 (4 loại backup, so sánh MinIO/Garage/rclone, đích lớp 2 ngoài nhà cung cấp), 14.8 (lộ trình hai bước Litestream → tự viết, thứ tự lớp bảo vệ); thêm Phụ lục D (kiểm kê toàn bộ phụ thuộc OSS + chiến lược tự viết theo 4 trục); phát hành kèm Kế hoạch quản lý code trên GitHub.
- **v2.5 → v2.6 (bản này):** thêm mục 16 — đa quốc gia theo kiến trúc cell; 16.4 chốt đặt tên phẳng theo tenant kiểu Slack (DNS định tuyến từng tenant về cell của nó, wildcard mặc định về nước gốc, chống trùng slug bằng chính DNS; luật redirect-không-proxy — bác phương án path `/cc` lẫn geo-routing kiểu Google), luồng bật quốc gia mới 5 bước; định vị lại mục 16 là decision record với 3 ràng buộc hôm nay — nước nhà = cell đầu tiên của người deploy, bộ adapter VN là reference implementation (mỗi nước một bản deploy độc lập, tái dùng cơ chế fork-and-deploy; latency được chứng minh không phải vấn đề nhờ edge-first, lý do tách là pháp lý; global rollup không PII cho tầm nhìn toàn cầu); bổ sung 4 việc kéo theo: i18n từ commit đầu, locale pack thành nhóm cấu hình hạng nhất, rollup theo múi giờ store, ngưỡng chuyển sang registry; phát hành kèm UI/UX guideline v1.

## Phụ lục C — Vì sao chọn từng công nghệ (và con số đứng sau)

Con số hiệu năng / tài nguyên / latency / CCU chi tiết: **mục 11**. Những gì đã cân nhắc và loại: **mục 12**. Bảng này bổ sung phần lý do chọn — tất cả là ước lượng thiết kế, pilot đo thật rồi cập nhật ngược vào mục 11.

| Công nghệ | Vì sao chọn | Đã cân nhắc & loại | Con số then chốt |
|---|---|---|---|
| Rust (edge + cloud) | Hiệu năng cỡ C, an toàn bộ nhớ, một binary tĩnh dễ phát hành, FFI tốt cho DLL máy cà thẻ | Go (đủ tốt nhưng FFI và độ gọn binary kém hơn), Node/Java (RAM, đóng gói) | pos_edge < 1% CPU ở tải quán |
| SQLite (WAL) | Nhúng trong tiến trình — không có dịch vụ DB phải cài/vá/giám sát trên 50–1000 máy; bền qua cúp điện | Postgres tại quán (thêm một thứ phải nuôi ở mỗi máy) | > 10.000 tx ghi/giây vs nhu cầu 1–3/giây |
| NATS JetStream | Outbound-only giải bài toán 4G/CGNAT; hàng đợi bền trên đĩa; cực nhẹ | MQTT broker (persistence yếu hơn), Kafka (nặng, JVM, quá cỡ bài toán) | 50–1000 kết nối dài hạn ≈ vài trăm MB RAM |
| PostgreSQL | Partition theo store + RLS + JSONB trong một node; hệ sinh thái backup/CDC chuẩn | 1000 DB riêng (bản v1 — vận hành nổ), MySQL (RLS/partition yếu hơn) | 2.000–5.000 insert/giây vs đỉnh ~40/giây @50 store |
| Bảng rollup trong Postgres | Dashboard luôn đọc bảng tổng hợp nhỏ | ClickHouse + Debezium (hai hệ thống nữa phải nuôi) | query < 10ms |
| SolidJS | Reactivity mịn, không Virtual DOM — mượt trên tablet/điện thoại cũ, bundle nhỏ | React (nặng hơn trên máy yếu) | UI nhúng, phục vụ LAN < 2ms |
| MinIO / Garage | S3 API tự host 0 đồng — Litestream nói chuyện thẳng | S3 thật (phí theo GB), NFS thô (không có API chuẩn) | 50 store ≈ 5–10GB |
| Litestream | Chép WAL liên tục lên S3 — linh hồn của "thay máy 5–10 phút" | Tự viết WAL-shipping (giữ làm phương án dự phòng — mục 14.8) | RPO vài giây |
| VictoriaMetrics + Grafana | Nhẹ hơn hẳn stack Prometheus/ELK đầy đủ | ELK (RAM lớn), SaaS monitoring (phí) | ≈ 0.7GB RAM |
| minisign (ed25519) | Ký OTA đơn giản, khóa tự quản, 0 đồng | Cert code-signing thương mại (phí thuê bao năm) | — |
| ULID | Sinh offline không cần hỏi ai, sắp xếp được theo thời gian, chống trùng khi đồng bộ | UUID v4 (không sort được), số tự tăng (trùng khi gộp nhiều quán) | — |


## Phụ lục D — Kiểm kê phụ thuộc & chiến lược tự viết

### D.1 Toàn bộ thành phần open-source đang dùng

**Cloud (`pos_cloud`)**

| Thành phần | Vai trò | Giấy phép |
|---|---|---|
| Rust + toolchain | Ngôn ngữ | MIT/Apache-2.0 |
| tokio | Async runtime | MIT |
| Axum (+ hyper, tower) | HTTP server | MIT |
| rustls / ring | TLS | Apache/ISC |
| serde | Serialize | MIT/Apache |
| sqlx / tokio-postgres | Driver Postgres | MIT/Apache |
| **PostgreSQL** | DB trung tâm | PostgreSQL License |
| **NATS JetStream** + async-nats | Message broker | Apache-2.0 |
| **MinIO** hoặc **Garage** | Object storage S3 | **AGPL-3.0** |
| **VictoriaMetrics** | Lưu metrics | Apache-2.0 |
| **Grafana** | Dashboard giám sát | **AGPL-3.0** |
| argon2, ed25519-dalek, sha2 | Mật mã | MIT/Apache |
| totp-rs | 2FA | MIT |
| minisign | Ký bản OTA | ISC |
| rclone | Backup lớp 2 | MIT |

**Edge (`pos_edge`)**

| Thành phần | Vai trò | Giấy phép |
|---|---|---|
| tokio, Axum, rustls, serde | Như trên | |
| **SQLite** + rusqlite | DB tại quán | Public domain |
| **Litestream** | Chép WAL lên cloud | Apache-2.0 |
| rust-embed | Nhúng UI vào binary | MIT |
| tokio-tungstenite | WebSocket realtime LAN | MIT |
| serialport-rs | Cổng COM (in, cà thẻ) | **MPL-2.0** |
| mdns-sd | Công bố `pos.local` | MIT |
| windows-service / systemd | Chạy nền | MIT |
| keyring / DPAPI-TPM | Cất credential | MIT |
| sntpc | Đồng bộ giờ | MIT |
| ulid-rs | Sinh ID | MIT |

**Frontend:** SolidJS (MIT), TailwindCSS (MIT), Vite + Node (chỉ dùng lúc build).

Khoảng 25 thành phần "có tên", kéo theo 200–400 crate phụ thuộc gián tiếp.

### D.2 Xếp hạng theo 4 trục (giấy phép · tài nguyên · hiệu năng · tối ưu)

Nguyên tắc phá thế cân bằng: **lợi ích ÷ bán kính thiệt hại khi có bug**. Thứ đáng viết là thứ mà bug chỉ làm mất *biểu đồ*; thứ không nên viết là thứ mà bug làm mất *tiền*.

**ĐÁNG VIẾT**

| # | Hạng mục | Được gì trên 4 trục | Bán kính thiệt hại |
|---|---|---|---|
| 1 | **WAL-shipping riêng** (thay Litestream) | Giấy phép: **xóa luôn MinIO/Garage** (2 thành phần AGPL nặng nhất) · Tài nguyên: bớt 300MB–1GB RAM + 1 tiến trình · RPO: vài giây → **< 1 giây** · Tối ưu: bỏ overhead HTTP/ký AWS của S3, nén zstd context chung, gộp khúc nhỏ → giảm băng thông 4G (tài nguyên đắt nhất ở edge) | Thấp — outbox mới là lớp bảo vệ doanh thu |
| 2 | **Dashboard riêng** (thay Grafana) | Giấy phép: AGPL thứ ba biến mất → **toàn hệ thống sạch AGPL** · Tài nguyên: ~200–400MB RAM · Perf: đọc thẳng bảng rollup < 10ms, phục vụ bởi HTTP server đã chạy sẵn | Bằng không (bug = biểu đồ xấu) |
| 3 | **Format/giao thức nhỏ**: ULID, TOTP, parse minisign, SNTP, mDNS | Giảm hàng chục crate gián tiếp (bề mặt supply-chain), binary nhỏ hơn, kiểm soát hoàn toàn. Mỗi cái 50–200 dòng | Thấp |
| 4 | **serialport-rs → gọi thẳng Win32/termios** | Xóa nốt MPL-2.0 (copyleft cấp file duy nhất còn lại); kiểm soát chính xác timeout cổng COM — có ý nghĩa thật với độ trễ máy cà thẻ | Thấp |

**Ranh giới tuyệt đối ở mục 3:** viết *format*, không viết *nguyên thủy mật mã*. Tự viết TOTP = viết phần truncation, HMAC/SHA-2/ed25519 vẫn dùng crate.

**KHÔNG NÊN VIẾT**

| Hạng mục | Lý do bằng con số / bản chất |
|---|---|
| **NATS** | Đỉnh nhu cầu ~400–800 event/giây @1000 store; NATS làm hàng triệu msg/giây → đang dùng **dưới 1% năng lực**. Lợi ích trên trục hiệu năng và latency = **0** (không thể nhanh hơn thứ đã dư 1000 lần). Giấy phép Apache-2.0, không vướng. Đổi lại phải tự dựng ack, redelivery, dedupe, backpressure, persistence, crash recovery — nơi trú ngụ của bug "mất đơn không rõ nguyên nhân sau 3 tháng" |
| **SolidJS** | Runtime ~7KB gzip, tự viết may lắm 3KB; Solid đã nằm nhóm nhanh nhất (fine-grained reactivity, Real DOM) → tự viết nhiều khả năng **chậm hơn**, tức lỗ trên chính trục ưu tiên |
| **tokio, hyper/Axum** | Scheduler work-stealing tối ưu nhiều năm; tự viết event loop gần như chắc chắn chậm hơn dưới tải. Axum vốn đã rất mỏng |
| **rclone** | Không lợi gì trên cả 4 trục: chạy theo cron, RAM ~0 khi ngủ, MIT |
| **SQLite, PostgreSQL, rustls, ed25519/argon2/SHA** | Nhóm mà **lỗi không biểu hiện khi test**: mật mã sai vẫn mã hóa/giải mã đúng, test xanh hết, chỉ kẻ tấn công biết nó rò khóa qua timing. SQLite có bộ test lớn gấp hàng trăm lần mã nguồn của chính nó, tích lũy từ hàng tỷ giờ chạy thật trên phần cứng lỗi và filesystem nói dối về `fsync` — không mua được bằng nỗ lực, chỉ bằng phơi nhiễm thực tế |

**PHÁT HIỆN NGƯỢC — VictoriaMetrics: đừng thay bằng bảng Postgres thường**

| | VictoriaMetrics | Bảng Postgres thô |
|---|---|---|
| Dung lượng mỗi điểm đo | ~0.5–1 byte (nén chuyên dụng) | ~40–60 byte (row + index) |
| @50 store, 30 metric/30s | ~3MB/ngày | **~200–400MB/ngày** |

Trên ổ 100GB, "tự viết cho gọn" ăn hết đĩa trong vài tháng — bỏ một service để tiết kiệm 300MB RAM, đổi lại đốt hàng trăm GB đĩa. Chỉ có **một** cách tự viết mà thắng: kho metrics dạng cột dùng delta-of-delta cho timestamp + XOR cho giá trị (thuật toán Gorilla, paper công khai), ~500 dòng, về trong khoảng 2× của VictoriaMetrics. Không định làm đúng bài này thì **giữ VictoriaMetrics**.

### D.3 Kết quả sau khi làm 4 mục đáng viết

Hệ thống **sạch hoàn toàn AGPL và MPL** · bớt 2 daemon · cloud nhẹ đi ~0.5–1.4GB RAM · RPO < 1 giây · băng thông 4G giảm nhờ nén riêng — và không mục nào có bug làm mất tiền.

### D.4 Lộ trình

| Giai đoạn | Làm gì |
|---|---|
| Năm 1 | Tầng sản phẩm (domain POS, sync, OTA, provisioning, dashboard…) + **định nghĩa toàn bộ port ngay từ commit đầu**, dùng thư viện ngoài phía sau port |
| Năm 2 | Thay các mục D.2 — bắt đầu bằng WAL-shipping (nó tự động xóa luôn Garage/MinIO: chiến thắng kép) |
| Năm 3 | Cân nhắc `MessageLink` riêng **chỉ khi** đã có fleet simulator và dữ liệu vận hành thật chứng minh nhu cầu. Sau khi làm xong WAL-shipping, ~60% của một giao thức riêng (khung tin, nén, phiên, retry) đã có sẵn nên chi phí biên giảm mạnh |
| Không bao giờ | Nhóm DB engine / TLS / nguyên thủy mật mã |

**Nguyên tắc rút gọn để tự áp cho quyết định sau này:** đáng viết khi thứ đó tồn tại *chỉ vì phải nói giao thức chung với bên ngoài* (S3, dashboard, format file) — bỏ giao thức đi là cả một tầng biến mất. Không đáng viết khi thứ đó *đã dư thừa năng lực* so với nhu cầu (NATS, tokio) — vì không còn gì để giành lại.

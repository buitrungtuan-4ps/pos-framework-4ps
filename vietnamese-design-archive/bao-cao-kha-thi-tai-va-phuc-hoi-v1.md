# Báo cáo khả thi, tải & phục hồi — v1

> **Phân loại:** T3 – Nội bộ. Tài liệu thứ 6, đi cùng Kiến trúc v2.6, Đặc tả nghiệp vụ v2, Chuẩn đặt tên v1, UI/UX v1, Kế hoạch quản lý code v2.
> **Phạm vi:** chạy lại toàn bộ luồng sau khi thêm module **QR ordering**; tính tài nguyên/đĩa/latency **không tính network latency** (chỉ compute + đĩa + lịch trình pipeline); dựng ma trận sự cố–phục hồi.
> **Cảnh báo diễn giải:** mọi con số là **ước lượng thiết kế** trên phần cứng mục tiêu (VPS 4 core / 24GB / NVMe cục bộ). Pilot đo thật rồi cập nhật ngược.

---

## 1. Kiểm kê luồng (đã chạy lại toàn bộ)

**Thiết lập:** fork → secrets → deploy → siêu quản trị → tenant → brand → store → export → kích hoạt thiết bị → ghép máy in/KDS/tablet.
**Bán hàng tại quán:** mở bàn/tab → gọi món (modifier, nửa–nửa, seat, course) → bắn bếp → KDS bump → tính tiền (tách/gộp, comp/discount/voucher/tip) → thanh toán đa phương thức → biên nhận → đóng bàn.
**Kênh ngoài:** đơn vendor (Grab/ShopeeFood) · `POST /v1/orders` từ kênh bán của tenant · **QR ordering (mới)** · giao vận (Ahamove/Grab Express).
**Vận hành:** ca & két (đếm mù) · tồn kho (BOM, khả dụng nấu, auto-86, kiểm kê) · khuyến mãi & voucher · RBAC · i18n · fiscal VN.
**Nền tảng:** đồng bộ outbox → NATS → Postgres → rollup · cấu hình versioned + hot-reload · OTA rings · thay máy 5–10 phút · webhook/API/feed · đa tenant subdomain · đa quốc gia (cell).

## 2. Ma trận kịch bản

| # | Kịch bản | Kết quả |
|---|---|---|
| 1–12 | Bộ kịch bản vòng trước (khai trương, ca qua nửa đêm, đổi giá giữa chừng, tách bill làm tròn, race thanh toán, capture thẻ treo, hóa đơn công ty, 86 vs đơn vendor, đổi menu trước peak, vòng đời tenant, nhân viên đa quán) | ✅ Qua sau khi vá mục 24–25 Đặc tả v2 |
| 13 | **QR: khách quét → gọi món → bếp nhận** | ✅ Qua — tái dùng `OrderIn`; bếp thấy đơn sau 0.5–2s |
| 14 | **QR khi quán offline** | ⚠️ Chức năng dừng → trang QR hiển thị "vui lòng gọi nhân viên" (thiết kế có chủ đích, mục 8-F2) |
| 15 | **QR khi cloud sập** | ⚠️ Như trên; quán vẫn bán bình thường bằng thao tác nhân viên |
| 16 | **QR đơn phá hoại từ ngoài quán** (QR bị chụp ảnh) | 🔴 → vá bằng F5: xác nhận nhân viên mặc định bật + rate limit theo bàn + chỉ trong giờ mở cửa |
| 17 | **QR + món vừa auto-86** | ✅ Menu QR đọc projection cloud (trễ 1–3s); đơn chứa món hết → theo luật vendor: từ chối món kèm thông báo |
| 18 | **QR + bàn đang có order của nhân viên** | ✅ Append vào cùng order của bàn; dòng gắn nguồn `channel=QR` để đối chiếu |
| 19 | **Stress: xả backlog 200 quán offline cả ngày** | ✅ ~9 phút, đệm ~1GB — trong giới hạn stream (14.11) |
| 20 | **Stress: QR spike 80% adoption trong 1 giờ** | ✅ ~2.7 đơn/s, ~2.400 phiên đồng thời — compute không đáng kể |
| 21 | **Cloud mất dữ liệu** | ✅ **Dựng lại bằng replay từ edge** (F4) — mỗi quán giữ 90 ngày |

## 3. Tài nguyên & hiệu năng — tại quán (mỗi store, 200 bill/ngày)

| Chỉ số | Giá trị |
|---|---|
| RAM `pos_edge` | 200–400MB |
| CPU | <1% trung bình, <5% giờ cao điểm |
| Đĩa | 150–300MB (90 ngày) + ~20MB ảnh menu + WAL |
| Ghi SQLite | 1–3 tx/s đỉnh (trần >10.000/s) |
| Thiết bị LAN | 3–30, WebSocket fanout <50ms |
| **QR ordering** | **0 tải thêm** — khách đi qua cloud, không chạm edge (chỉ nhận đơn qua NATS như vendor) |

## 4. Tài nguyên & hiệu năng — cloud (3 quy mô, đã gồm QR)

| | **A** 300 quán · 200 bill · QR 30% | **B** 1.000 quán · 500 bill · QR 50% | **C** 400 quán nhỏ · 80 bill · QR 20% |
|---|---|---|---|
| Sự kiện/ngày | 480k | 4M | 256k |
| Ingest đỉnh | 27/s (giật 60–80) | 222/s (giật 500–700) | 25/s |
| Phiên QR/ngày | 9k | 250k | 6k |
| QR đồng thời (đỉnh) | ~150 | ~4.200 | ~100 |
| HTTP req đỉnh | ~40/s | ~200/s | ~30/s |
| CPU | <10% | 30–50% | <5% |
| RAM | 9–10GB | 12–16GB | 8–9GB |
| **Đĩa/ngày** | 290MB → 9GB/tháng | **2,4GB → 72GB/tháng** | 160MB |
| **Băng thông/ngày** | 9–15GB | **~250GB (7,5TB/tháng)** | 6–10GB |
| Kết luận | VPS mục tiêu dư sức | Cần đĩa 500GB–1TB **và** kiểm tra hạn mức chuyển dữ liệu | Hạ tầng "ngủ" |

**Ràng buộc mới do QR:** băng thông. Trước QR, cloud chỉ phục vụ admin (vài GB/tháng). Xem F1 để hạ xuống.

## 5. Latency (compute + đĩa + lịch trình — không tính network)

| Đường đi | Thời gian |
|---|---|
| Chạm → ghi SQLite → màn khác nhận (trong quán) | **1–4ms** |
| Sự kiện quán → dashboard cloud (full flow) | compute 5–25ms · gồm lịch trình 60–300ms |
| **QR: submit đơn → cloud xử lý** | **5–15ms** |
| **QR: cloud → NATS → edge → KDS hiện đơn** | **0,5–2s** (chủ yếu lịch trình + fsync) |
| QR: tải menu (server-side) | 2–10ms (đọc cache/rollup) |
| Webhook: sự kiện → POST rời cloud | +100–300ms sau ingest |
| API đọc (rollup) | <10ms |
| Hot-reload cấu hình | <1s |

## 6. Stress & giới hạn

| Bài | Kết quả |
|---|---|
| Burst ingest 700/s (kịch bản B đỉnh) | ~30% trần ghi Postgres; fsync NVMe là yếu tố quyết định |
| 200 quán offline cả ngày rồi nối lại | 800k sự kiện xả trong ~9 phút, đệm ~1GB |
| Endpoint webhook chết 24h rồi sống lại | Cursor tụt, drain có trần tốc độ, không dồn RAM |
| QR spike ×2,7 lần bình thường | Compute không đáng kể; băng thông ×2,7 → xem F1 |
| 1.000 quán cùng nhận cấu hình mới | 1.000 message tức thì; mỗi quán áp <1s |
| Fanout OTA ring 2 (500 quán tải cùng lúc) | Artifact 50MB × 500 = 25GB — **rải theo lô** (đã có trong rings) |

## 7. Ma trận sự cố & phục hồi

| Sự cố | Bán kính | Phát hiện | Hoạt động thay thế | Phục hồi | Mất dữ liệu |
|---|---|---|---|---|---|
| Máy chủ quán chết | 1 quán ngừng bán | Nhân viên tức thì; heartbeat 30–60s | — | Thay máy + mã kích hoạt: **5–10 phút** | ≤ RPO WAL (giây); outbox đã sync |
| Mất điện giữa giao dịch | 1 quán | Khi khởi động lại | UPS nếu có | Vài giây (SQLite WAL recovery) | Chỉ transaction chưa commit |
| Đĩa quán đầy | 1 quán | Cảnh báo ngưỡng (14.11) | — | Phút | Không |
| Mạng quán đứt | Quán **vẫn bán**; vendor busy; **QR dừng** | Heartbeat | Nhân viên ghi đơn tay | Tự động khi có mạng | Không |
| **Cloud VPS sập** | Dashboard, QR, webhook, ingest dừng — **mọi quán vẫn bán** | Ping ngoài (14.9) / báo cáo | Quán tự trị hoàn toàn | Restore 30–60 phút | ≤ RPO backup (xem F3) |
| Postgres hỏng | Toàn bộ dữ liệu cloud | Kiểm tra toàn vẹn | Quán tự trị | Restore, hoặc **replay từ edge (F4)** | ≤ 90 ngày phục hồi được |
| JetStream đầy | Sync dừng, sự kiện đọng ở outbox | Cảnh báo độ sâu | — | Phút | Không |
| Bản OTA lỗi | ≤ kích thước ring | Self-test + canary | Auto-rollback + kill-switch | Phút | Không (có bản copy DB trước update) |
| Máy in/KDS hỏng | 1 trạm | Hàng đợi in + badge đỏ | Máy in dự phòng / màn KDS | Ngay | Không |
| Máy cà thẻ mất kết nối | Thanh toán thẻ thủ công | Nhân viên | Tiền mặt/QR; bill treo → đối soát | Ngay | Không |
| NCC hóa đơn điện tử sập | Hóa đơn dồn hàng đợi | Độ sâu queue | Phát hành offline theo dải số | Theo NCC | Không |
| **QR bị lạm dụng (đơn phá hoại)** | 1 quán, bếp | Nhân viên thấy đơn lạ | **Xác nhận nhân viên (F5)** | Ngay | Không |
| Chứng chỉ TLS hết hạn | HTTPS + QR dừng | Giám sát | — | Caddy tự gia hạn | Không |
| Lệch/giả mạo đồng hồ edge | Mốc thời gian, ranh giới ca | SNTP + cảnh báo lệch | ClockSource port | Ngay | Bất thường vào audit |

## 8. Phát hiện mới & đề xuất (từ vòng chạy này)

| # | Phát hiện | Đề xuất |
|---|---|---|
| **F1** | **Băng thông cloud thành ràng buộc mới** vì QR phục vụ ảnh menu cho khách (kịch bản B: 7,5TB/tháng) | Thumbnail ≤30KB + lazy-load + `Cache-Control: immutable` theo URL băm; nếu cần thì đẩy **riêng ảnh** lên CDN — hợp lệ pháp lý vì **ảnh món không chứa PII** |
| **F2** | **Cloud lần đầu nằm trên đường đi của khách hàng** — QR sập khi cloud/mạng quán sập | Tuyên bố suy giảm rõ ràng: trang QR hiện "vui lòng gọi nhân viên"; nhân viên luôn là đường lùi. Không hứa SLA khách-hàng-cuối cho QR |
| **F3** | RPO cloud hiện phụ thuộc nhịp backup (có thể tới 24h) | Bật **WAL archiving liên tục** cho Postgres ra Garage → RPO xuống **phút** |
| **F4** | **Cloud dựng lại được từ các quán** — mỗi edge giữ 90 ngày sự kiện | Thêm API nội bộ "reset cursor + replay từ ULID" → mất dữ liệu cloud ≤90 ngày là phục hồi được. Tính chất phục hồi mạnh, chi phí gần bằng 0 |
| **F5** | QR tĩnh bị chụp ảnh → đơn phá hoại từ xa | **Xác nhận nhân viên mặc định BẬT** + rate limit theo bàn + chỉ nhận trong giờ mở cửa + chỉ khi quán online |
| **F6** | Thanh toán trực tuyến cho QR kéo theo scope lớn | v1: **trả tại quầy**. Cổng thanh toán online = nhóm adapter thứ 6, làm sau |
| **F7** | Ảnh menu cần kích thước thứ hai cho QR | Pipeline ảnh sinh 2 bản: thumbnail ≤30KB (danh sách) + ảnh ≤150KB (chi tiết) |

## 9. Kết luận khả thi

- **Kịch bản A và C:** một VPS mục tiêu chạy dư sức, mọi tầng dưới 10% công suất.
- **Kịch bản B (1.000 quán bận):** khả thi trên một VPS về CPU/RAM, nhưng **phải** nâng đĩa (500GB–1TB) và kiểm tra hạn mức băng thông; đây là hai tường tuyến tính duy nhất, định cỡ trước được bằng công thức mục 11 Kiến trúc.
- **QR ordering:** khả thi và rẻ về kiến trúc (tái dùng `OrderIn`), nhưng đổi *tính chất* hệ thống — cloud trở thành thành phần khách hàng nhìn thấy. Đây là đánh đổi có ý thức, không phải lỗi thiết kế.
- **Khả năng chịu lỗi:** không sự cố nào trong ma trận mục 7 làm **ngừng bán hàng tại quán** ngoài hỏng phần cứng chính của quán đó — đúng cam kết offline-first. Cloud sập = mất tính năng quản trị và QR, không mất doanh thu.

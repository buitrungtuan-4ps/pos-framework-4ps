# UI/UX Guideline — v1

> **Phân loại:** T3 – Nội bộ. Đi cùng Kiến trúc v2.6 và Đặc tả nghiệp vụ POS v1.
> **Người dùng thật của UI này:** nhân viên đứng quầy/bếp 8 tiếng, tay ướt hoặc dính dầu, thao tác trong giờ cao điểm, trên thiết bị đủ loại từ điện thoại tới màn KDS nhìn xa 2 mét. Mọi quyết định thiết kế phục vụ người đó trước, người ngồi văn phòng sau.

---

## 0. Triết lý: "Khung tối giản, nội dung đậm đặc, không bất ngờ"

Đối chiếu bộ định hướng đã chốt:

| Định hướng | Áp dụng thế nào |
|---|---|
| Modern Minimal | Minimal ở **trang trí** (không gradient/bóng đổ rườm rà, không hiệu ứng trình diễn) — nhưng KHÔNG minimal về **mật độ thông tin** ở màn thu ngân/KDS: người vận hành cần thấy nhiều thứ cùng lúc |
| Nhẹ, phản hồi nhanh | Nền tảng đã cho <5ms phía server; phần UI cam kết thêm: mọi chạm phản hồi trên màn hình <100ms (nguyên tắc 1) |
| Đa device / device adaptive | Cùng một app, **layout riêng theo lớp thiết bị** — không phải một layout co giãn (nguyên tắc 9) |
| Adaptive UX / context aware / context driven | Adaptive theo *vai trò, trạng thái hệ thống, thời điểm ca* — với ràng buộc cứng ở nguyên tắc 3: vị trí điều khiển chính không bao giờ đổi |

## 1. Mười nguyên tắc

1. **Tốc độ cảm nhận là tính năng số 1.** Optimistic UI: chạm là UI cập nhật ngay, đồng bộ chạy sau (outbox lo phần đúng đắn). Skeleton thay spinner; không bao giờ có màn hình trắng chờ. Animation ≤ 150ms và chỉ để định hướng — tuyệt đối không animation trong đường thao tác tiền.
2. **Vùng chạm cho ngón tay thật:** tối thiểu 48px; phím số nhập tiền và hành động chính 56–64px; khoảng cách giữa các vùng chạm ≥ 8px. Không hover-only, không gesture ẩn làm đường chính.
3. **Vị trí ổn định — muscle memory là tài sản.** Nút thanh toán, bắn bếp, mở bàn không bao giờ đổi chỗ theo ngữ cảnh. Adaptive được phép thay đổi *nội dung và thứ tự ưu tiên hiển thị*, không được thay đổi *vị trí điều khiển chính*.
4. **Context-aware có kỷ luật.** Theo **vai trò**: màn hình đầu của phục vụ là sơ đồ bàn, của thu ngân là hàng chờ thanh toán, của bếp là KDS trạm mình. Theo **trạng thái**: offline/máy in lỗi hiện ngay tại chỗ liên quan. Theo **thời điểm**: cuối ca nổi lối kết ca. Cấm: ẩn chức năng đang cần để "cho gọn".
5. **Trạng thái hệ thống luôn nhìn thấy được:** một thanh mảnh thường trực hiển thị online/offline, hàng đợi in, sync lag — không popup, không che nội dung. Offline không phải lỗi, là một trạng thái làm việc bình thường (mục 5).
6. **Mỗi màn hình trả lời đúng một câu hỏi** (khớp luật "mỗi màn hình một query" của kiến trúc). Thao tác thường ≤ 2 chạm, thao tác hiếm ≤ 3 chạm tính từ màn hình vai trò.
7. **Lỗi luôn có lối ra.** Mọi error state kèm hành động kế tiếp (thử lại · chuyển máy in khác · gọi quản lý bằng PIN). Không dead-end, không hiện mã lỗi trần cho nhân viên — mã kỹ thuật vào log.
8. **Số tiền là vua:** cỡ chữ lớn nhất màn hình, font tabular (chữ số đều cột), phân tách nghìn theo locale, không bao giờ hiển thị khác giá trị thật vì làm tròn thẩm mỹ.
9. **Bốn lớp thiết bị, bốn layout:**
   - *POS chính* (màn ≥ 13"): 2 cột — trái danh mục/món, phải bill đang mở.
   - *Tablet order*: lưới món lớn, bill trượt lên; tối ưu một tay cầm một tay chạm.
   - *Điện thoại*: 1 cột, hành động chính neo đáy màn trong tầm ngón cái.
   - *KDS*: đọc từ 2 mét — chữ ≥ 24–28px, tương phản cao, nền tối (đỡ chói trong bếp), thao tác bump bằng chạm to hoặc bump-bar; theme sáng/tối là cấu hình theo thiết bị (7.1).
10. **i18n từ commit đầu** (yêu cầu trực tiếp của kiến trúc đa quốc gia — mục 16): không hardcode chuỗi nào; ngày/số/tiền render theo locale của store; layout chịu được text dài hơn 30% (tiếng Đức, tiếng Thái) không vỡ; ngôn ngữ là cấu hình theo store và có thể theo thiết bị.

## 2. Design tokens

| Nhóm | Giá trị |
|---|---|
| Spacing | Thang 4px: 4 · 8 · 12 · 16 · 24 · 32 |
| Vùng chạm | 48px chuẩn · 56–64px cho phím tiền/hành động chính |
| Chữ | 12 (phụ) · 14 (nhãn) · 16 (thân) · 20 (đề mục) · 28 (KDS) · 40+ tabular (tổng tiền) |
| Màu ngữ nghĩa | Thành công / cảnh báo / lỗi / thông tin — đạt tương phản WCAG AA; **không truyền nghĩa chỉ bằng màu** (luôn kèm icon hoặc chữ — 8% nam giới mù màu đỏ-lục) |
| Bo góc & nét | Một mức bo (8px), một mức nét (1px) — đủ, không thêm |
| Chuyển động | 100–150ms ease-out, chỉ cho xuất hiện/định hướng |
| Theme | Sáng / Tối toàn hệ, cấu hình theo thiết bị (KDS mặc định tối); token màu tách khỏi token cấu trúc |
| Bàn phím ảo | Component dùng chung, bật khi nhập text/số trên thiết bị chạm không có bàn phím vật lý (tên tab, tên khách, ghi chú, số lượng) |

Toàn bộ tokens sống trong một file theme của UI (SolidJS + Tailwind config) — đổi nhận diện thương hiệu theo tenant sau này là đổi token, không sửa component.

## 3. Spec bốn màn hình lõi

**Order (phục vụ — tablet/điện thoại).** Sơ đồ bàn là màn hình gốc: màu trạng thái bàn, chạm bàn → order. Lưới món theo category, món hết (86) mờ + gạch ngay lập tức trên mọi thiết bị. Thêm món = 1 chạm; modifier bắt buộc mở ngay khi chạm món có yêu cầu; món **nửa–nửa** chọn hai nửa trong cùng một luồng (một dòng bill, hiện rõ hai phần). Khi cờ `seats` bật: chọn ghế trước khi thêm món, dòng món hiện số ghế. Mỗi món hiển thị **khả dụng nấu** ("còn ~6") tính từ tồn nguyên liệu dùng chung, cập nhật < 50ms sau mỗi lần bắn. Nút "Bắn bếp" cố định, hiện số dòng chưa bắn. Hai người cùng sửa một bàn: dòng của người kia xuất hiện realtime kèm nhãn tên.

**Thu ngân (POS chính).** Cột phải là bill: dòng món, giảm giá, phí, VAT, **tổng to nhất màn**. Bàn phím số lớn cho tiền mặt, hiện tiền thối chữ to ngay khi nhập. **Quick cash:** hàng nút mệnh giá nhanh (50k/100k/200k/"đủ tiền") một chạm thay vì gõ số. **Tip** (khi cờ `tips` bật): hàng nút % gợi ý + nhập tay, hiện *sau* khi chọn phương thức thẻ. **Open item:** nút nhập món tự do (tên + giá), có kiểm quyền. Các phương thức thanh toán là hàng nút to cố định. Tách bill = kéo dòng món sang panel mới. Thao tác cần quyền mở ô nhập PIN tại chỗ — không điều hướng đi đâu. Vào màn thanh toán = **khóa mềm bill** + thiết bị khác mở cùng bill thấy "X đang thanh toán"; màn hình tự khóa sau N phút không thao tác, mở lại bằng PIN.

**KDS (bếp).** Thẻ theo order/course, đồng hồ đếm trên từng thẻ, đổi màu theo ngưỡng thời gian cấu hình. Bump = chạm cả thẻ (vùng chạm là toàn thẻ). Recall giữ 60 giây gần nhất ở mép màn. Phiếu HỦY hiện đỏ + giữ 10 giây kèm âm báo. Không có element trang trí nào — màn này là công cụ sản xuất.

**Store profile quyết định màn khởi đầu và flow** (Đặc tả v2 mục 18): FnB = sơ đồ bàn, trả sau · Cafe-quầy = order tại quầy, trả trước, số gọi khách · Retail = quét mã, trả ngay. Cùng bộ component, khác cách lắp — không phải ba app.

**Dashboard admin (web, đa quốc gia).** Màn "Hôm nay" đọc từ bảng rollup (<10ms): doanh thu, số bill, theo giờ, theo phương thức. Heatmap fleet xanh/đỏ. Mọi bảng cấu hình có nút "xem store nào đang ở version nào". Chấp nhận mật độ thông tin cao — người dùng ở đây là quản trị, không phải nhân viên quầy.

### 3.1 Danh mục màn hình dashboard (kiểm kê đủ/thiếu)

Đã đặc tả trong bộ tài liệu: setup wizard · cây cấu hình 6 nhóm · heatmap fleet · "Hôm nay" · trạng thái hệ thống · audit log · quản lý thiết bị + thu hồi · duyệt máy in đề xuất · link mời · export store. **Cần đặc tả tiếp (backlog có tên):** ① quản lý phát hành OTA (tạo release, tiến độ rings, nút kill-switch) · ② viewer đối soát đêm + ngoại lệ thanh toán "không rõ kết quả" · ③ trang chi tiết store (health, version, backup gần nhất, độ sâu queue HĐĐT, nút kéo log trực tiếp) · ④ báo cáo chuỗi theo khoảng ngày · ⑤ nhân sự cấp tenant nhìn xuyên store · ⑥ hành động khôi phục (cấp lại mã kích hoạt, reset lease) · ⑦ Developers theo tenant (API key + scope, webhook endpoint, log giao hàng N ngày + nút redeliver, gửi sự kiện thử) · ⑧ Localization (bật ngôn ngữ theo tenant/brand/store, lưới dịch EN → đích, % hoàn thành, export/import CSV). Cơ chế nền của cả 6 đều đã có trong kiến trúc — đây là product work, không phải hạ tầng.

## 4. Offline & degraded — spec trạng thái

| Tình huống | UI thể hiện |
|---|---|
| Mất mạng | Thanh trạng thái chuyển "Offline — vẫn bán bình thường"; đếm số sự kiện chờ đồng bộ; KHÔNG chặn bất kỳ thao tác bán hàng nào |
| Máy in trạm lỗi | Badge đỏ tại thẻ KDS trạm đó + lối ra: "In lại · Chuyển máy in dự phòng"; hàng đợi in hiển thị số phiếu chờ |
| Quá ngưỡng offline (chính sách 7.1) | Banner cảnh báo → mức tiếp theo yêu cầu PIN quản lý xác nhận mỗi ca — đúng thang leo thang đã chốt, không tự chặn bán |
| Thanh toán thẻ "không rõ kết quả" | Bill treo trạng thái vàng, hướng dẫn 2 lựa chọn (xác nhận tay theo máy cà thẻ / hủy giao dịch), tự vào danh sách đối soát |
| Máy chủ quán khởi động lại | Client tự nối lại qua pos.local, hiện "Đang nối lại…" ≤ vài giây, bàn và ca giữ nguyên |

### 3.2 Thao tác chạm cho thiết bị không bàn phím
Máy POS/tablet chạm thường không có bàn phím vật lý → **bàn phím ảo trên màn** là thành phần dùng chung: bàn phím số cho tiền/số lượng, bàn phím chữ cho tên tab/khách/ghi chú. Gọi ngay tại chỗ nhập, không điều hướng. Vùng phím ≥ 48px (mục nguyên tắc 2).

## 5. Những gì cố tình KHÔNG làm

Trang trí theo trend (glassmorphism, parallax…) · onboarding tour dài (thay bằng menu mẫu + checklist sẵn sàng để học bằng cách dùng) · tooltip dày đặc · gesture ẩn làm đường thao tác chính · dark pattern giữ chân · popup xác nhận cho thao tác đảo ngược được (undo thay cho confirm ở thao tác chưa bắn bếp/chưa thanh toán) · nhạc/hiệu ứng âm ngoài các âm báo nghiệp vụ (bump, lỗi in, phiếu hủy).

---

**Thước đo hoàn thiện của tài liệu này:** một nhân viên mới cầm thiết bị lần đầu, bán được đơn hoàn chỉnh trong **5 phút không cần đào tạo**; và một thu ngân lành nghề thao tác cả ca **không bao giờ phải nhìn tìm nút** — hai thước đo này kiểm chứng trong pilot bằng quan sát thật, không bằng khảo sát.

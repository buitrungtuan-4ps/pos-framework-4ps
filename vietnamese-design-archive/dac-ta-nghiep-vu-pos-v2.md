# Đặc tả nghiệp vụ POS — v2 (bộ tính năng đầy đủ cho framework)

> **Phân loại:** T3 – Nội bộ. Đi cùng Kiến trúc v2.6 và Chuẩn đặt tên & API v1 (`chuan-dat-ten-va-api-v1.md`). Thay thế bản v1.
> **Mọi định danh, tên trường, tên sự kiện, mã quyền trong tài liệu này tuân theo Chuẩn đặt tên & API v1.**
> **v2 bổ sung (mục 15–20):** RBAC cấu hình được, tồn kho + định mức BOM, voucher, store profile (FnB/Cafe/Retail), gói chống gian lận. Toàn bộ chạy trên kiến trúc hiện có — không thêm thành phần hạ tầng nào.
> **Nguyên tắc của bản này:** đây là bộ tính năng CHUẨN của một POS F&B, mỗi tính năng ở dạng đơn giản nhất mà vẫn dùng thật được ngoài quán. Không có trong tài liệu này = không làm ở bản 1 (danh sách loại trừ ở mục 12). Core trung lập quốc gia — hóa đơn pháp lý là module cắm sau (mục 14).

---

## 0. Phạm vi & vai trò

Ba luồng bán: **phục vụ tại bàn** (dine-in), **mang đi** (takeaway), **đơn app giao đồ ăn** đổ vào từ vendor. Năm vai trò: Phục vụ · Thu ngân · Bếp · Quản lý (ca/store) · Admin chuỗi (chỉ trên dashboard cloud). Menu, sơ đồ bàn, thuế/phí, khuyến mãi, quyền — tất cả cấu hình từ cloud (kiến trúc mục 7.1); tài liệu này chỉ mô tả *hành vi tại quán*.

---

## 1. Bàn & khu vực

- Sơ đồ bàn theo khu vực (tầng, sân…); trạng thái mỗi bàn: **Trống → Đang phục vụ → Chờ thanh toán → Chờ dọn → Trống**.
- **Mở bàn:** chạm bàn trống → tạo order gắn bàn; nhập số khách (tùy chọn, phục vụ báo cáo).
- **Chuyển bàn:** chuyển toàn bộ order sang một bàn trống khác (ghi log ai chuyển).
- **Gộp bàn:** gộp order của hai bàn thành một; từng dòng món vẫn giữ dấu vết bàn gốc để bếp không loạn.
- Một bàn = một order đang mở tại một thời điểm.

## 2. Gọi món (order)

- Thêm món từ menu theo category; mỗi **dòng món** gồm: số lượng, modifier/topping (danh sách cấu hình theo món, có thể bắt buộc chọn — ví dụ size), ghi chú tự do ("không hành").
- **Course:** mỗi dòng gắn course (Khai vị / Món chính / Tráng miệng — mặc định Món chính). Bắn bếp theo từng course hoặc cả order.
- **Bắn bếp (fire):** dòng *chưa bắn* thì sửa/xóa thoải mái. Dòng *đã bắn* muốn hủy → cần PIN Quản lý + chọn lý do + **in phiếu HỦY tại đúng trạm bếp** của món đó.
- **Giữ món (hold):** đánh dấu dòng chưa bắn dù order đã lưu — bắn sau bằng tay.
- **Hết món (86):** Quản lý (hoặc Bếp, tùy cấu hình) đánh dấu hết → món mờ đi trên **mọi thiết bị ngay lập tức** và tự báo "tạm hết" sang vendor giao đồ ăn; mở bán lại một chạm.
- Mọi thao tác trên order là **lệnh ghi thêm (append)**: hai người trên hai thiết bị cùng thêm món vào một bàn thì hai lệnh tự hòa vào nhau, không ghi đè nhau (nền tảng: kiến trúc mục 14.1). Sửa/xóa *cùng một dòng* từ hai máy: lệnh sau thắng, cả hai đều nằm trong nhật ký.

## 3. Bếp: KDS & in phiếu

- Dòng món bắn đi được **định tuyến tới trạm** theo luật cấu hình món → trạm (pizza / bar / dessert…). Bản 1: một món thuộc đúng một trạm.
- **Màn KDS mỗi trạm:** thẻ hiển thị theo order + course, đồng hồ chờ trên từng thẻ, tự đổi màu khi vượt ngưỡng X phút (cấu hình). **Bump** = xong món/thẻ; **Recall** = gọi lại thẻ vừa bump nhầm.
- **In phiếu bếp** chạy song song với KDS hoặc thay thế KDS (quán nhỏ chỉ dùng máy in). Phiếu hủy in chữ **HỦY** to, kèm lý do.
- **Expo** (tùy chọn bật ở quán lớn): một màn tổng hợp các món đã bump theo bàn để nhân viên chạy món.
- Máy in trạm hỏng → lệnh in tự chuyển máy in dự phòng đã cấu hình, hoặc dồn cảnh báo đỏ lên KDS trạm đó (kiến trúc mục 14.2).

## 4. Tính tiền & thanh toán

- **Bill:** tạm tính → trừ giảm giá → cộng phí phục vụ (% cấu hình) → VAT (cấu hình, giá đã gồm hay chưa gồm thuế là thuộc tính cấu hình) → làm tròn theo quy tắc cấu hình → tổng.
- **Tách bill**, đúng 2 cách ở bản 1: (a) **theo món** — kéo từng dòng sang bill mới; (b) **chia đều N phần**. **Gộp bill:** gộp về một trước khi thanh toán.
- **Giảm giá:** theo dòng món hoặc cả bill; theo % hoặc số tiền; luôn chọn lý do. Vượt ngưỡng cấu hình → cần PIN Quản lý.
- **Thanh toán** — 4 phương thức, cho phép **kết hợp nhiều phương thức trên một bill**:
  - *Tiền mặt:* nhập tiền khách đưa → hiện tiền thối → mở két.
  - *Thẻ:* đẩy đúng số tiền sang máy cà thẻ, chờ kết quả; nhánh "không rõ kết quả" (timeout) → bill treo trạng thái chờ, giải quyết bằng đối soát (kiến trúc mục 4.2).
  - *QR/ví:* hiện QR kèm số tiền; xác nhận tay hoặc tự động nếu có callback.
  - *Ghi công nợ/khác:* một phương thức "khác" có ghi chú, cho tình huống lẻ.
- **Sau thanh toán:** in biên nhận (hoặc không in — một chạm); **số biên nhận tăng liền mạch theo store** (mục 14). In lại bất kỳ lúc nào, bản in lại đóng dấu **BẢN SAO**.
- **Void bill / hoàn tiền sau khi đã thanh toán:** chỉ Quản lý; chọn lý do; in phiếu void; tiền mặt hoàn tại két (ghi thành khoản chi trong ca), thẻ ghi nhận hoàn để đối soát.

## 5. Mang đi & đơn app giao đồ ăn

- **Takeaway:** order không gắn bàn, nhận **số gọi khách** tăng theo ngày, in kèm phiếu; luồng bếp giống hệt dine-in.
- **Đơn vendor (Grab/ShopeeFood):** đổ vào như order nguồn "vendor" → vào thẳng KDS đúng trạm; thao tác xác nhận/từ chối trong SLA hiển thị ngay trên POS; nút "sẵn sàng giao"; in phiếu dán túi. Quán offline → vendor tự được chuyển sang chế độ bận (kiến trúc mục 8).

## 6. Ca & két tiền

- **Mở ca:** nhập tiền đầu ca. Mọi giao dịch, giảm giá, hủy món đều gắn vào ca đang mở của máy đó.
- **Thu / chi trong ca (paid-in / paid-out):** nhập số tiền + lý do (mua đá, nộp tiền về két lớn…).
- **Kết ca:** đếm tiền thật → hệ thống hiện **chênh lệch** so với sổ; báo cáo ca gồm: doanh thu theo phương thức, số bill, trung bình bill, tổng giảm giá, tổng hủy món, void, thu/chi. Kết ca xong ca khóa lại, chỉ xem.
- **Mở két ngoài giao dịch:** cần PIN Quản lý, ghi log lý do.
- Bản 1: mỗi máy thu ngân một ca đang mở tại một thời điểm.

## 7. Khuyến mãi — engine tối giản, đúng 3 dạng

| Dạng | Hành vi | Ví dụ |
|---|---|---|
| Giảm món/nhóm | Giảm % hoặc số tiền cho món/category | -20% pizza thứ 3 hằng tuần |
| Khung giờ (happy hour) | Tự áp trong khung giờ + ngày cấu hình | Đồ uống -30% 14–17h |
| Combo giá cố định | Chọn đủ N món định sẵn → giá gói X | 1 pizza + 1 salad + 2 nước = 399k |

Điều kiện chung: khung giờ, thứ trong tuần, store áp dụng. Chế độ **tự động áp** hoặc **thu ngân chọn tay**. Cấu hình 100% từ cloud, đổi là quán nhận < 1 giây. Một bill hiển thị rõ từng khoản khuyến mãi đã áp.

## 8. Ma trận quyền (mặc định — cloud chỉnh được)

| Hành động | Phục vụ | Thu ngân | Quản lý |
|---|---|---|---|
| Mở bàn, thêm món, bắn bếp | ✓ | ✓ | ✓ |
| Hủy dòng CHƯA bắn | ✓ | ✓ | ✓ |
| Hủy dòng ĐÃ bắn | PIN QL | PIN QL | ✓ |
| Chuyển/gộp bàn | ✓ | ✓ | ✓ |
| Giảm giá trong ngưỡng | — | ✓ | ✓ |
| Giảm giá vượt ngưỡng | — | PIN QL | ✓ |
| Thanh toán, in biên nhận | — | ✓ | ✓ |
| Tách/gộp bill | — | ✓ | ✓ |
| Void bill / hoàn tiền | — | — | ✓ |
| Mở/kết ca, thu-chi | — | ✓ | ✓ |
| Mở két ngoài giao dịch | — | — | ✓ |
| Đánh dấu hết món (86) | tùy cấu hình | ✓ | ✓ |

PIN nhập ngay trên thiết bị đang thao tác (Quản lý không cần chạy về máy chính). Mọi hành động cần quyền đều vào nhật ký: ai, lúc nào, lý do.

## 9. Báo cáo tại quán (tối thiểu)

Một màn "Hôm nay": doanh thu, số bill, trung bình bill, doanh thu theo phương thức, theo giờ; top món bán; tổng giảm giá & hủy món kèm danh sách. Báo cáo nhiều ngày / so sánh chuỗi: xem trên dashboard cloud. **Khoảng ngày tùy chọn (day / month / custom range) + export CSV** ở mọi báo cáo cloud — cho kế toán và phân tích ngoài.

## 10. Hành vi chuẩn khi mất mạng

Toàn bộ mục 1–9 chạy **nguyên vẹn** khi offline, với đúng ba ngoại lệ: (a) đơn vendor không vào được (vendor tự thấy quán bận); (b) thanh toán thẻ tùy máy cà thẻ — máy standalone vẫn quẹt được, thu ngân ghi nhận phương thức "thẻ" bằng tay, đối soát sau; (c) không tra cứu được dữ liệu chỉ có trên cloud. Khi có mạng lại, mọi thứ tự đồng bộ, không ai phải làm gì.

## 11. Hai luồng chuẩn end-to-end

```
DINE-IN
Mở bàn 12 → thêm món (2 thiết bị cùng lúc vẫn ổn) → bắn course 1
→ KDS trạm nhận, nấu, bump → chạy món → khách gọi thêm → bắn tiếp
→ xin tính tiền → (tách bill / giảm giá nếu cần) → thanh toán tiền mặt + thẻ
→ in biên nhận (số liền mạch) → bàn sang Chờ dọn → dọn xong → Trống
```

```
TAKEAWAY / ĐƠN APP
Tạo đơn mang đi (hoặc đơn Grab tự đổ vào) → số gọi khách → vào KDS
→ bump → "sẵn sàng" → thanh toán tại quầy (đơn app: đã thanh toán sẵn)
→ in phiếu dán → giao khách / shipper
```

## 12. Ngoài phạm vi bản 1 (cố tình không làm)

Đặt bàn trước · buffet/tính giờ · ~~trừ tồn theo định mức~~ (nay: mục 16) · CRM & tích điểm · ~~voucher mã~~ (nay: mục 17) · đa tiền tệ trong một bill · ~~phiếu bếp đa ngôn ngữ~~ (nay: mục 21.2) · thiết bị cân · tự do kéo-thả sơ đồ bàn tại quán (sơ đồ chỉnh trên cloud) · món một-nhiều-trạm · đặt món tự phục vụ (QR order của khách).

## 13. Sự kiện phát lên cloud (chính là outbox ở kiến trúc mục 7)

Đặt tên theo `domain.resource.action` (Chuẩn đặt tên mục 5), envelope thống nhất `event_id`/`event_type`/`event_time`/`schema_version` + ngữ cảnh `tenant_id`/`brand_id`/`store_id`/`device_id`/`employee_id`/`shift_id`:

`sales.order.opened` · `sales.order_line.added` / `.updated` / `.voided` / `.fired` · `sales.table.transferred` / `.merged` · `kitchen.ticket.bumped` / `.recalled` · `billing.bill.split` / `.merged` / `.settled` / `.voided` · `billing.discount.applied` · `billing.comp.applied` · `billing.payment.captured` · `billing.tip.adjusted` · `billing.refund.issued` · `cash.shift.opened` / `.closed` · `cash.drawer.opened` / `.paid_in` / `.paid_out` · `inventory.item.sold_out` / `.restored` · `inventory.stock.consumed` / `.adjusted` / `.counted` · `promotion.voucher.redeemed` · `delivery.shipment.created` / `.status_changed`.

Đây là nguồn duy nhất cho báo cáo chuỗi, đối soát và posting sang ERP.

## 14. Số biên nhận & module tài khóa quốc gia

- **Core:** mỗi bill thanh toán xong nhận **số biên nhận tăng liền mạch theo store** — bộ đếm nằm trong *cùng transaction SQLite* với bill, nên offline vẫn liền mạch tuyệt đối, không nhảy số trong vận hành bình thường.
- **Module tài khóa theo quốc gia** (cắm qua port Fiscalization, làm sau khi core chạy): quyết định số hóa đơn pháp lý, ký số, gửi cơ quan thuế, và chính sách xử lý các tình huống biên (ví dụ thay máy) theo luật từng nước. Việt Nam là module đầu tiên; core không đổi khi thêm quốc gia.


---

# PHẦN II — Bổ sung v2

## 15. RBAC đầy đủ — vai trò là dữ liệu cấu hình

Quyền là **danh mục cố định của framework**; vai trò = tập quyền + tham số, **tạo/sửa trên cloud** (cây cấu hình 7.1), sync xuống quán → check quyền tại edge, offline hoạt động y hệt. Mỗi quyền có thể gắn cờ "cần PIN tại chỗ". Mọi hành động nhạy cảm ghi audit: ai, vai trò, lý do, thiết bị.

**Danh mục quyền (nhóm):**

| Nhóm | Quyền |
|---|---|
| Bán hàng | mở đơn/bàn · thêm món · sửa/xóa dòng CHƯA bắn · bắn bếp · hold · hủy dòng ĐÃ bắn · chuyển bàn · gộp bàn · đánh dấu 86 |
| Thanh toán | tách bill · gộp bill · giảm giá trong trần vai trò · giảm giá vượt trần · đổi voucher · price override (có trần riêng) · thu tiền/settle · in lại biên nhận · void bill đã thanh toán · refund |
| Két & ca | mở ca · kết ca · thu-chi trong ca · mở két ngoài giao dịch · xem báo cáo ca |
| Menu & tồn kho | nhập kho · điều chỉnh tồn · ghi hao hụt · kiểm kê · xem giá vốn |
| Quản trị quán | xem báo cáo quán · ghép thiết bị · xem nhật ký |
| Cloud-only | quản lý menu/giá/khuyến mãi/voucher · quản lý nhân sự & vai trò · cấu hình thiết bị/máy in · API key & webhook · phát hành OTA · export store · báo cáo brand/tenant |

**Tham số theo vai trò:** trần giảm giá % / số tiền · trần price-override · cờ cần-PIN từng quyền. **Template mặc định** (sửa được): Phục vụ · Thu ngân · Ca trưởng · Quản lý quán · Quản lý brand · Tenant admin · Kế toán (chỉ đọc) · Kiểm toán (chỉ đọc audit).

**Chuẩn hóa tự động khi dev thêm quyền — permission registry là nguồn chân lý:** mỗi quyền là một khai báo trong `pos-core` kèm metadata (id dạng `domain.resource.action`, nhóm, mô tả, mức rủi ro, vai trò mặc định, cờ cần-PIN). **Deny-by-default.** Thêm quyền = thêm một khai báo → compile bắt cập nhật template (match exhaustive) → dashboard tự hiện trong trình sửa vai trò (đọc catalog) → audit tự gắn nhãn → bảng ma trận ở trên **tự sinh từ registry**, không bảo trì tay. Kỷ luật CI: **snapshot danh mục quyền** — thêm quyền là diff thấy được trong PR; xóa quyền bị cấm (chỉ deprecate — id quyền là hợp đồng như tên sự kiện). Thi hành qua đúng một hàm `require(permission)`; CI cấm check vai trò bằng chuỗi ad-hoc.

## 16. Tồn kho & định mức (BOM)

**Dữ liệu:** Nguyên liệu (đơn vị: g / ml / cái; category nguyên liệu riêng, khác category món bán) · **Định mức theo món VÀ theo modifier** (pizza size L = định mức gốc + 50g bột) · Sổ tồn per-store dạng sự kiện: tiêu hao (tự động), nhập kho, điều chỉnh, hao hụt, kiểm kê (đặt lại số thực đếm).

**Luật vận hành:**
1. **Trừ tại thời điểm BẮN BẾP** (nguyên liệu đã dùng), không phải lúc thanh toán. Hủy-sau-bắn → ghi **hao hụt**, không cộng lại tồn (config được per-tenant).
2. **Real-time đúng nghĩa:** projection cục bộ tại quán cập nhật < 50ms trên mọi màn hình; **auto-86** khi nguyên liệu dưới ngưỡng (tự ẩn món dùng nguyên liệu đó + báo "tạm hết" sang vendor giao đồ ăn); cloud projection trễ 1–3s cho báo cáo brand. Không bao giờ sync "số tồn" hai chiều — giữ nguyên luật sự kiện của kiến trúc mục 7.
3. **Hai chế độ per-tenant:** *ERP-led* (SAP giữ master + nhập kho, POS phát tiêu hao — mô hình chuỗi lớn) · *Standalone* (framework tự có nhập kho tay, kiểm kê định kỳ, hao hụt, cảnh báo ngưỡng — cho tenant không có ERP).
4. Retail dùng chung sổ tồn với BOM 1:1 theo SKU.

**Khả dụng nấu (available-to-make) — nguyên liệu dùng chung:**
```
khả_dụng(món) = floor( min qua mọi nguyên liệu ( tồn[i] ÷ định_mức[món][i] ) )
```
Khi bắn bếp trừ nguyên liệu X, chỉ tính lại các món có X trong BOM (menu 200 món, nguyên liệu chung ~20 món → ~100 phép chia = micro giây) → đẩy "còn ~N" lên mọi màn order < 50ms; chạm ngưỡng → auto-86 + báo vendor. Ví dụ: C=10, D=8, E=6; A cần D+E → khả dụng 6; B cần C+D → 8; nấu 1 A (D→7, E→5) → A còn 5 **và B tụt xuống 7 dù không bán B** — vì D dùng chung. Cloud dựng cùng projection từ sự kiện tiêu hao (trễ 1–3s) cho brand-view: mỗi quán một cột + tổng brand phục vụ mua hàng. Ghi chú trung thực: đây là số **lý thuyết** — rơi vãi làm nó trôi dần; kiểm kê là hành động kéo về sự thật, hai thứ là một cặp.

## 17. Pricing & Promotion Engine — một mô hình cho mọi thứ giảm giá

Happy hour, giảm món/nhóm, combo, voucher, giảm tay là **một mô hình duy nhất**:

```
Campaign = phạm vi (tenant/brand/store) + lịch (khung giờ, thứ)
         + điều kiện (món/nhóm, bill tối thiểu, kênh bán)
         + hành động (−% / −tiền / giá combo / tặng món)
         + luật cộng dồn (nhóm loại trừ, thứ tự ưu tiên)
         + quota (số lượt, ngân sách) + [voucher codes đính kèm]
```

**Thứ tự đánh giá tất định:** khuyến mãi mức món → combo → mức bill → voucher → giảm tay. Mọi dòng áp được hiện tường minh trên bill. Cấu hình 100% từ cloud, hot-reload < 1 giây.

**Nguyên tắc offline của engine (luật framework):** cái gì là **LUẬT** thì sync xuống và chạy offline nguyên vẹn (happy hour, promo tự động, combo, giảm tay); cái gì cần **TÍNH DUY NHẤT toàn cục** thì online (voucher). **Đổi voucher = gọi cloud, atomic check-and-mark** → đổi trùng bất khả thi, không cần đối soát. Mất mạng: nút voucher mờ kèm thông báo, mọi thứ khác bán bình thường.

## 18. Loại hình cửa hàng (store profile) — màn hình & flow theo mô hình

Profile = một bó cấu hình trong cây 7.1: màn hình khởi đầu, thứ tự flow, bật/tắt tính năng. Đổi profile không đổi code.

| | FnB phục vụ bàn | Cafe / quầy (QSR) | Retail *(data ngày 1, UI phase 2)* |
|---|---|---|---|
| Màn khởi đầu | Sơ đồ bàn | Màn order tại quầy | Ô quét mã vạch |
| Thanh toán | Trả SAU, tách/gộp bill | **Trả TRƯỚC**, số gọi khách | Trả ngay tại quầy |
| Bếp | KDS đa trạm + course | KDS 1 trạm hoặc chỉ in phiếu | Không |
| Tồn kho | BOM theo định mức | BOM đơn giản | 1:1 theo SKU |
| Bàn | Có | Tắt (hoặc sơ đồ tối giản) | Không |
| Tab tên khách (bar/pub) | Tùy chọn | Cờ `tabs` (order mở có nhãn tên, trả cuối) | Không |

Item mang sẵn trường **SKU / barcode / variant** từ ngày 1 — bật retail sau không phải migrate dữ liệu.

**Hiện thực bằng capability flags:** profile không phải ba giao diện — là **một bộ cờ năng lực** trong cây cấu hình (`tables`, `tabs`, `seats`, `kds`, `courses`, `pay_first`, `barcode`, `queue_number`, `tips`…); UI lắp từ các khối, mỗi màn hình khai báo cờ nó cần. FnB/Cafe/Retail chỉ là **ba preset đặt tên sẵn** — tenant chọn preset rồi ghi đè từng cờ (quán lai hợp lệ). Hai kỷ luật: (a) cờ đọc qua một **CapabilityContext** trung tâm, cấm rải `if(flag)` khắp code; (b) **luật hợp lệ giữa các cờ** (`pay_first=true` → `tables` tắt) — cloud validate trước khi áp, config hỏng giữ bản tốt cuối. Đổi cờ = hot-reload < 1 giây.

## 19. Gói chống gian lận

1. **Kết ca đếm mù:** thu ngân nhập số tiền đếm được *trước khi* hệ thống hiện số kỳ vọng → chênh lệch mới lộ ra. Chặn tận gốc "đếm cho khớp".
2. **Lý do bắt buộc** (danh mục config từ cloud) cho: hủy món đã bắn · giảm giá · refund · void bill · mở két ngoài giao dịch.
3. **Phân tích theo nhân viên** trên dashboard: tỷ lệ hủy / giảm giá / refund / mở két / in lại so với mặt bằng đồng nghiệp → cờ bất thường tự động.
4. In lại biên nhận: dấu BẢN SAO + đếm số lần + cần quyền. Price override: quyền riêng, có trần theo vai trò.
5. Voucher đổi online-atomic → không thể trùng (mục 17); dashboard vẫn có báo cáo đổi voucher theo nhân viên/campaign. Audit bất biến + đồng hồ NTP → khớp được với camera của quán nếu có (điểm tích hợp, không phải tính năng).
6. **Cố tình KHÔNG làm "training mode"** — chế độ bán-không-ghi-sổ là véc-tơ gian lận kinh điển (bán thật, in bill tập, tiền vào túi). Đào tạo bằng store demo + menu mẫu.

## 20. Ghi chú phạm vi v2

Chuyển khỏi danh sách loại trừ: tồn kho định mức (→ mục 16), voucher (→ mục 17). Vẫn loại trừ có chủ đích: đặt bàn trước · buffet/tính giờ · CRM & tích điểm · đa tiền tệ trong một bill · ~~QR khách tự order~~ (nay: mục 26) · training mode (lý do ở 19.6) · cân trọng lượng (xem xét cùng retail phase 2). Toàn bộ mục 15–19 chạy trên sự kiện + cây cấu hình sẵn có — **không yêu cầu thay đổi kiến trúc**.


## 21. Đa ngôn ngữ (i18n) — tiếng Anh là gốc, mọi ngôn ngữ khác là lớp phủ

### 21.1 Hai tầng chuỗi

| | Chuỗi UI framework | Chuỗi nội dung tenant |
|---|---|---|
| Là gì | Nút, nhãn, thông báo lỗi | Tên món, category, modifier, tên khuyến mãi, template biên nhận |
| Sở hữu | Framework: base EN + pack (vi/ja…) phát hành kèm OTA; tenant ghi đè được từng khóa qua config | **Tenant admin trên cloud** |
| Lưu | Nhúng trong binary (rust-embed) | Trường dịch trong cây cấu hình: `{"en": bắt buộc, "vi": …}` — sync + hot-reload < 1s, offline nguyên vẹn |

**Luật fallback:** tiếng Anh luôn có mặt làm gốc → không bao giờ hiện ô trống/khóa kỹ thuật; thiếu bản dịch → hiện EN + tính vào % hoàn thành trên dashboard.

### 21.2 Độ phân giải ngôn ngữ

Nhân viên (tùy chọn, theo PIN đăng nhập) → thiết bị → store → brand → tenant → `en`. **Ngôn ngữ khách tách riêng** (biên nhận, màn hình khách) theo cấu hình store — ví dụ quán tại Nhật: nhân viên thao tác tiếng Việt, biên nhận tiếng Nhật. Phiếu bếp/KDS theo ngôn ngữ **của trạm** — "phiếu bếp đa ngôn ngữ" chuyển khỏi danh sách loại trừ (nay gần như miễn phí nhờ hệ thống này).

### 21.3 Quản lý trên cloud

Màn **Localization** theo tenant: bật ngôn ngữ theo tenant/brand/store → lưới EN ↔ ngôn ngữ đích, lọc "chưa dịch", % hoàn thành → export/import CSV cho người dịch ngoài. Nút "dịch nháp bằng AI" là tích hợp tùy chọn, không phải core (tránh phụ thuộc + chi phí API — đúng luật kết nạp).

### 21.4 Chuẩn kỹ thuật

Khóa đặt tên `domain.screen.element`, append-only. Định dạng thông điệp theo **ICU MessageFormat** (nội suy + số nhiều — key-value thô gãy ngay khi ra khỏi tiếng Việt/Anh). Ngày/số/tiền theo locale pack (Kiến trúc 16.5). **CI gác chuỗi hardcode** (Kế hoạch GitHub, luật code #6). **Gotcha máy in nhiệt:** phần lớn máy ESC/POS không có font Unicode đầy đủ (dấu tiếng Việt, CJK) → dòng chữ ngoài bảng mã máy in được **render thành bitmap** trước khi in — chậm vài ms, đúng trên mọi máy, không phụ thuộc codepage từng hãng.

### 21.5 Phạm vi trung thực

RTL: layout không được chặn RTL, hỗ trợ đầy đủ để dành khi có thị trường thật. Ngôn ngữ tài liệu/code của dự án giữ nguyên quyết định ở Kế hoạch GitHub mục 12.


## 22. Bổ sung feature-cấp-quầy (đối chiếu POS chuẩn)

### 22.1 Running tabs (tab theo tên khách)
Cờ năng lực `tabs` (mục 18): order mở **không gắn bàn, gắn nhãn tên/số khách**, cho phép gọi nhiều lượt rồi thanh toán cuối — dùng cho bar/pub/quầy. Cùng luật append và bếp như order thường; khác dine-in ở chỗ định danh bằng nhãn thay vì bàn.

### 22.2 Nhóm khách (customer groups) — KHÔNG phải CRM
Nhóm khách (nhân viên / VIP / thành viên…) quản trên cloud, phục vụ đúng hai việc: (a) **điều kiện trong Pricing Engine** (mục 17) — giá/chiết khấu theo nhóm; (b) **chiều lọc báo cáo** doanh thu theo nhóm. **Ranh giới cứng:** chỉ nhóm + điều kiện giá + chiều báo cáo; KHÔNG lịch sử mua của từng khách, KHÔNG tích điểm, KHÔNG hồ sơ cá nhân (đó là CRM — vẫn hoãn, mục 20). Chọn nhóm tại POS là một chạm khi mở/thanh toán bill.

### 22.3 Sinh PDF menu (tính năng cloud)
Xuất menu ra PDF từ dashboard, tái dùng dữ liệu món + ảnh (Kiến trúc 14.12) + đa ngôn ngữ (mục 21): chọn brand/store + ngôn ngữ → PDF in được hoặc trang menu **chỉ-xem** qua QR. Bản 1 chỉ xem, KHÔNG cho order (đặt món tự phục vụ vẫn hoãn — mục 20) để không kéo theo scope QR-order.


## 23. Đối chiếu POS thương mại (Toast / Square) — bổ sung

### 23.1 Bảy hạng mục ảnh hưởng DATA MODEL (quyết ngay, đắt nếu vá sau)

| Hạng mục | Đặc tả |
|---|---|
| **Tip (tiền boa)** | Trường `tip_amount` **tách khỏi** tiền hàng trên mỗi payment; sự kiện `TipAdjusted` cho phép sửa tip *sau khi* đã capture thẻ (bắt buộc cho thị trường Mỹ/Nhật); tip tiền mặt khai báo cuối ca; báo cáo chia tip theo ca/nhân viên. UI bật/tắt theo locale — VN mặc định tắt, model luôn có |
| **Số ghế (seat)** | Trường `seat` trên dòng món (cờ năng lực `seats`); cho phép **tách bill theo ghế** — cách tách tự nhiên của fine dining, bổ sung cho tách-theo-món và chia-đều |
| **Lớp thuế + thuế theo kênh** | `tax_class` trên item (thực phẩm / đồ uống / rượu…) + bảng thuế trong locale pack theo **kênh bán** — ví dụ Nhật: mang đi 8%, tại chỗ 10% cùng một món. Thay cho VAT phẳng mức store |
| **Bảng giá theo kênh** | Giá gốc riêng cho dine-in / takeaway / từng vendor giao đồ ăn (bù hoa hồng). Là **price list**, KHÔNG phải khuyến mãi — Pricing Engine áp *sau* khi đã chọn bảng giá |
| **Comp ≠ Discount ≠ Void** | `comp` = tặng (vẫn trừ tồn, ghi chi phí, cần quyền + lý do) · `discount` = giảm giá · `void` = chưa từng xảy ra. Ba loại tách bạch trong sự kiện và báo cáo — kế toán và chống gian lận xử lý khác nhau |
| **Modifier nâng cao** | Nhóm modifier bắt buộc/tùy chọn, lồng nhau, và loại **`split_item` (nửa–nửa)** cho pizza: một dòng món chia phần theo tỷ lệ, quy tắc giá cấu hình, **mặc định: lấy giá cao nhất trong hai nửa** (tùy chọn: trung bình / cộng phụ thu), **BOM tính theo tỷ lệ phần** |
| **Open item** | Món tự do nhập tên + giá tại quầy; quyền riêng `sales.item.open`, luôn vào audit (véc-tơ gian lận nếu thả lỏng) |

### 23.2 Năm hạng mục mức cấu hình

Menu theo khung giờ (**dayparts** — món *có bán hay không* theo giờ, khác happy hour vốn chỉ đổi giá) · **Tùy biến biên nhận** (logo, chân trang, lời nhắn, theo brand) · **Màn hình hướng khách (CFD)** — thêm một loại thiết bị vào model thiết bị sẵn có · **Điều tiết đơn online** (trần đơn/15 phút khi bếp quá tải, đẩy prep-time sang vendor) · **Chấm công nhẹ** (clock in/out gắn PIN đăng nhập + báo cáo giờ công; KHÔNG làm bảng lương).

### 23.3 Cố tình KHÔNG copy từ Toast/Square

| Thứ | Lý do |
|---|---|
| Xử lý thanh toán (payment processing) | Là mô hình doanh thu của họ (% mỗi giao dịch). Mình giữ **trung lập thanh toán**: chỉ tích hợp terminal → không lock-in, dùng được acquirer nội địa mỗi nước, phần mềm 0 đồng. Đánh đổi: không có doanh thu payment, mỗi nước cần adapter terminal |
| Loyalty / marketing / CRM | Domain riêng, nặng; API mở cho phép tenant cắm hệ sẵn có |
| Đặt bàn / waitlist | Chưa xác nhận nhu cầu |
| Kiosk tự phục vụ, online store | **`POST /v1/orders` (Kiến trúc 17.4) đã cho phép bên thứ ba tự xây** — lợi thế của việc mở API sớm |
| Gift card | Cần sổ số dư online-atomic như voucher → **chừa sẵn kiểu payment method**, chưa hiện thực |
| Bảng lương, purchase order đầy đủ | Domain ERP/HR — đã có chế độ ERP-led (mục 16) |


## 24. Luật đúng-đắn-dữ-liệu (chốt trước khi code state machine)

1. **Ngày kinh doanh (`business_date`):** mỗi store có **giờ chốt ngày** cấu hình trong cây store (mặc định 04:00 giờ địa phương; quán bán ban ngày có thể đặt 00:00). Edge đóng dấu `business_date` lên mọi sự kiện; rollup, báo cáo, kết ca chạy theo `business_date` — bill 01:30 thuộc doanh thu tối hôm trước. **Module tài khóa dùng ngày lịch** (ngày pháp lý của hóa đơn). Hai khái niệm, hai trường, không trộn.
2. **Snapshot lên dòng món:** dòng món chụp lại giá + thuế (`tax_class` + mức tại thời điểm) + tên hiển thị + kết quả khuyến mãi **tại thời điểm thêm dòng** — không tham chiếu menu sống; menu đổi/xóa không ảnh hưởng order đang mở. Thời điểm đánh giá Pricing Engine: **mức món & combo tại lúc thêm dòng · mức bill & voucher tại lúc bắt đầu thanh toán.** (Hệ quả: gọi món 16:59 hưởng happy hour dù thanh toán 17:30 — đúng chuẩn ngành; kèm chiều báo cáo bất thường cho pattern lách giờ.)
3. **Làm tròn khi tách bill:** tổng các phần sau tách = **đúng tổng gốc đến từng đơn vị tiền**; phần dư làm tròn dồn vào phần cuối; giảm giá/voucher/phí mức bill đã áp thì phân bổ theo tỷ trọng từng phần. Bất biến CI: `sum(splits) == original_total`.
4. **Settle độc quyền:** `bill:settle` là chuyển trạng thái một-lần — yêu cầu thứ hai nhận `FAILED_PRECONDITION` (bất biến state machine, Kiến trúc 15.5). UX: vào màn thanh toán đặt **khóa mềm** + thông báo realtime cho thiết bị khác đang mở cùng bill.

## 25. Vòng đời tenant & các luật biên còn lại

**Vòng đời tenant — framework cấp CƠ CHẾ, người vận hành đặt CHÍNH SÁCH.**

Ba thao tác chuẩn trên console nền tảng: **Suspend** (khóa dịch vụ, giữ dữ liệu, quán chuyển read-only) · **Export** (toàn bộ dữ liệu tenant ra gói tải được) · **Delete** (ẩn danh PII + xóa dữ liệu vận hành, giữ bản ghi tài chính).

**Ranh giới:** framework **không mã hóa cứng con số năm nào** và không đưa ra phán quyết pháp lý — người fork về vận hành mới là bên kiểm soát dữ liệu. Thời hạn lưu là **tham số cấu hình** (mặc định theo locale pack từng nước, chỉnh theo luật sở tại). Framework chỉ chịu trách nhiệm một bảo đảm kỹ thuật: **ẩn danh xong thì số liệu tài chính vẫn khớp** (doanh thu, đối soát, hóa đơn không đổi).

**Hệ quả thiết kế bắt buộc — PII phải TÁCH ĐƯỢC (quyết trước khi chốt schema sự kiện):** event log là bất biến, nên **không nhúng PII vào payload sự kiện**. PII (tên/SĐT/email khách giao hàng, `buyer_*`) nằm ở bảng riêng khóa bởi `subject_id`; sự kiện chỉ mang `subject_id`. Ẩn danh = xóa một hàng, log không phải viết lại. Phương án tương đương: mã hóa PII bằng khóa riêng theo subject rồi hủy khóa (crypto-shredding). Không quyết điều này ngay thì "xóa dữ liệu một khách" sau này = viết lại toàn bộ lịch sử + mọi backup, tức bất khả thi.

**Các luật biên:**
- Đơn vendor chứa món vừa 86 (race 1–3s sync): config per-vendor — *từ chối cả đơn* hoặc *nhận thiếu món kèm thông báo*.
- **Refund/void chỉ thực hiện tại store gốc** của bill (dữ liệu per-store).
- **Nhân viên thuộc tenant, vai trò gán theo store** — làm nhiều quán = nhiều assignment, một `employee_id`, một PIN (băm sync xuống các store được gán).
- **Khóa màn hình thiết bị** sau N phút không thao tác (config theo store), mở bằng PIN — chặn mượn phiên; mọi thao tác gắn `employee_id` đang đăng nhập.
- **Kiểm kê giữa ca:** ghi `counted_qty` + `count_time`; hệ áp delta so với projection *tại thời điểm đếm* — bán hàng sau lúc đếm không làm sai kết quả kiểm kê.
- **Chính sách PIN nhân viên:** độ dài tối thiểu theo cấu hình, không trùng trong cùng store, **khóa 5 phút sau 5 lần sai** (áp cả khi offline), mọi lần sai vào audit. Mã kích hoạt thiết bị và setup token: rate limit + hết hiệu lực sau N lần thử sai.
- **Nhập menu hàng loạt:** import CSV/Excel ở bước tạo brand (xem trước, báo lỗi theo dòng, ánh xạ cột) — điều kiện để chuỗi đang dùng POS khác chuyển sang mà không nhập tay hàng nghìn món.
- **Thông tin người mua trên bill** (tùy chọn, là PII — mask trong log): `buyer_name`, `buyer_tax_code`, `buyer_email` — nguồn cho hóa đơn công ty của module tài khóa.


## 26. QR Ordering (module cloud)

### 26.1 Luồng
Khách quét QR dán tại bàn → mở web app (không cài đặt) do **cloud** phục vụ → chọn ngôn ngữ (mục 21) → xem menu + giá theo bảng giá kênh `QR` → đặt món → submit → cloud validate → publish qua NATS → **edge nhận như đơn kênh ngoài, tái dùng port `OrderIn`** → (tùy chọn) nhân viên xác nhận → bắn bếp/KDS. Đơn QR **append vào order của bàn** nếu bàn đang mở; mỗi dòng gắn `channel = QR` để đối chiếu.

### 26.2 Ranh giới có chủ đích
- **Thanh toán v1: trả tại quầy.** Cổng thanh toán trực tuyến là nhóm adapter thứ 6, làm sau.
- **QR là tính năng cloud, không offline-first:** quán mất mạng hoặc cloud sập → trang QR hiển thị "vui lòng gọi nhân viên"; nhân viên luôn là đường lùi. Không cam kết SLA khách-hàng-cuối cho QR.
- Menu QR đọc projection cloud (trễ 1–3s so với quán) — đơn chứa món vừa hết xử lý theo đúng luật vendor: từ chối món kèm thông báo.

### 26.3 An ninh & chống lạm dụng (QR dán tĩnh)
QR tĩnh (thực dụng cho vận hành) nên phải chặn đơn phá hoại từ xa: **xác nhận của nhân viên trước khi bắn bếp — mặc định BẬT** (cấu hình per-store) · **rate limit theo bàn** · chỉ nhận đơn **trong giờ mở cửa** của store · chỉ nhận khi **quán đang online** · phiên QR có hạn, gắn `table_id` đã ký. Số điện thoại khách (tùy chọn, để báo trạng thái) là PII → lưu theo `subject_id` tách khỏi event log (mục 25).

### 26.4 Cấu hình
Cờ `qr_ordering_enabled` theo store; bảng giá kênh `QR`; cờ `qr_staff_confirmation_required` (mặc định bật); giờ nhận đơn; số món tối đa mỗi lần gửi; ngôn ngữ mặc định của trang khách.

### 26.5 Sự kiện
`sales.qr_session.started` · `sales.order.submitted_by_guest` · `sales.order.confirmed_by_staff` · `sales.order.rejected_by_staff` — theo Chuẩn đặt tên mục 5.

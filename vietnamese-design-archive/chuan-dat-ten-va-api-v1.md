# Chuẩn đặt tên & API — v1

> **Phân loại:** T3 – Nội bộ. Tài liệu thứ 5, đi cùng Kiến trúc v2.6, Đặc tả nghiệp vụ v2, Kế hoạch quản lý code, UI/UX guideline.
> **Nền tảng:** Google API Improvement Proposals (AIP) — theo sát ở phần lớn, có **4 sai lệch có chủ đích** ghi ở mục 12.
> **Luật tối cao:** *một luật duy nhất trên toàn hệ* — `snake_case` cho mọi thứ đi trên dây và nằm trong kho: JSON, URL, cột DB, tên sự kiện, khóa cấu hình, nhãn metrics, mã quyền.

---

## 1. Nguyên tắc

1. **Một luật, không ngoại lệ theo tầng.** Cùng một khái niệm phải mang **cùng một tên** ở API, DB, sự kiện, log và tài liệu. `store_id` là `store_id` ở mọi nơi.
2. **Rust nội bộ theo chuẩn Rust** (`snake_case` cho field/hàm, `PascalCase` cho type, `SCREAMING_SNAKE` cho hằng) — ánh xạ 1:1 sang dây bằng `#[serde(rename_all = "snake_case")]`, không cần lớp chuyển đổi.
3. **Tên là hợp đồng.** Đã công bố thì chỉ được *thêm*, không đổi/xóa — chỉ deprecate (cùng luật với API snapshot, schema sự kiện và danh mục quyền).
4. **Không viết tắt** trừ danh sách trắng: `id`, `url`, `api`, `sku`, `vat`, `qr`, `pos`, `kds`, `erp`, `ip`, `ttl`, `utc`.

## 2. Tài nguyên & định danh

| Quy tắc | Ví dụ |
|---|---|
| Collection số nhiều, snake_case | `/v1/orders`, `/v1/order_lines`, `/v1/price_lists` |
| Khóa chính mang **tên đầy đủ**, không dùng `id` trần | `order_id`, `store_id`, `menu_item_id` |
| Khóa ngoại **giữ nguyên tên** khóa chính được tham chiếu | `orders.store_id` → `stores.store_id` |
| Giá trị định danh | ULID (26 ký tự, sắp xếp theo thời gian) |
| Phân cấp thể hiện bằng trường, không bằng đường dẫn lồng sâu | `{"tenant_id":…, "brand_id":…, "store_id":…}` |

## 3. Trường dữ liệu (JSON & DB dùng chung tên)

- **snake_case**, danh từ, không giới từ/mạo từ: `discount_reason` (không phải `reason_of_discount`).
- **Không nhét kiểu vào tên**: `note` (không phải `note_string`).
- **Số nhiều cho mảng**: `payments`, `order_lines`.
- **Boolean là tính từ/quá khứ phân từ, không có tiền tố `is_`**: `enabled`, `voided`, `fired`, `settled`. Cờ năng lực dùng hậu tố `_enabled`: `tables_enabled`, `tips_enabled`, `pay_first_enabled`.
- **Hậu tố đơn vị bắt buộc khi có đơn vị**: `_time`, `_duration_ms`, `_count`, `_bytes`, `_ratio`, `_percent`, `_amount_minor`, `_weight_grams`, `_volume_ml`.

### 3.1 Thời gian
Mọi mốc thời gian kết thúc bằng `_time`, kiểu **RFC 3339 UTC** (`2026-08-14T03:12:45.123Z`): `create_time`, `update_time`, `event_time`, `fire_time`, `settle_time`, `close_time`. Thời lượng ghi rõ đơn vị: `prep_duration_ms`. Cấm `created_at` / `updated_at`.

### 3.1b Trường chuẩn bổ sung
`business_date` (kiểu `DATE`, tính theo giờ chốt ngày của store — khác `event_time`) · `buyer_name` / `buyer_tax_code` / `buyer_email` (PII — mask trong log) · `counted_qty` + `count_time` (kiểm kê) · `subject_id` (trỏ tới bản ghi PII lưu tách; **sự kiện không bao giờ mang PII trực tiếp** — Đặc tả v2 mục 25).

### 3.2 Tiền
```json
{ "currency_code": "VND", "amount_minor": 150000 }
```
`currency_code` = ISO 4217; `amount_minor` = **số nguyên** theo đơn vị nhỏ nhất (VND: đồng; JPY: yên; USD: cent). Trường tiền trong DB: `bigint` + `char(3)`. Cấm số thực ở mọi tầng (luật code #1).

### 3.3 Enum
Giá trị `UPPER_SNAKE_CASE`, **luôn có giá trị 0 là `*_UNSPECIFIED`**, thêm giá trị mới là thao tác cộng:
```
ORDER_STATE_UNSPECIFIED · ORDER_STATE_OPEN · ORDER_STATE_SETTLED · ORDER_STATE_VOIDED
PAYMENT_METHOD_UNSPECIFIED · PAYMENT_METHOD_CASH · PAYMENT_METHOD_CARD ·
PAYMENT_METHOD_QR · PAYMENT_METHOD_VOUCHER · PAYMENT_METHOD_GIFT_CARD · PAYMENT_METHOD_OTHER
```
Bên nhận **bắt buộc** xử lý giá trị lạ như `*_UNSPECIFIED` thay vì lỗi — điều kiện để thêm enum không phá tương thích.

## 4. HTTP API

**Phương thức chuẩn:** `GET /v1/orders` (list) · `GET /v1/orders/{order_id}` (get) · `POST /v1/orders` (create) · `PATCH /v1/orders/{order_id}` (update, kèm `update_mask`) · `DELETE …`.

**Custom method dùng dấu hai chấm** (AIP-136): `POST /v1/bills/{bill_id}:void` · `:refund` · `:split` · `:redeliver` · `:rotate_secret`. Phân biệt rõ *hành động* với *tài nguyên con*.

**Phân trang** (AIP-158): tham số `page_size`, `page_token`; đáp `next_page_token`. Áp cho cả Event Feed:
```
GET /v1/events?page_size=200&page_token=<ulid>&filter=type:"billing.bill.settled"
```
**Lọc & sắp xếp:** `filter`, `order_by` (`order_by=create_time desc`). **Cập nhật một phần:** `update_mask=name,price_amount_minor`.

**Header:**

| Header | Dùng làm gì |
|---|---|
| `idempotency-key` | Chống trùng khi tạo (chuẩn ngành) |
| `pos-signature` | HMAC-SHA256 của webhook |
| `pos-signature-time` | Mốc thời gian ký (chống replay ±5 phút) |
| `pos-event-id` / `pos-delivery-id` | ULID sự kiện / lần giao |
| `pos-api-version` | Tùy chọn, ghim phiên bản phụ |

**Lỗi** (AIP-193) — một hình dạng duy nhất:
```json
{ "error": { "code": 400, "status": "INVALID_ARGUMENT",
             "message": "price_amount_minor must be positive",
             "details": [ { "field": "price_amount_minor", "reason": "MUST_BE_POSITIVE" } ] } }
```
`status` dùng mã chuẩn: `INVALID_ARGUMENT` · `NOT_FOUND` · `ALREADY_EXISTS` · `PERMISSION_DENIED` · `UNAUTHENTICATED` · `FAILED_PRECONDITION` · `RESOURCE_EXHAUSTED` · `INTERNAL` · `UNAVAILABLE`.

## 5. Sự kiện & webhook

**Tên sự kiện = `domain.resource.action`**, snake_case, action ở **quá khứ** — cùng taxonomy với mã quyền RBAC:

```
sales.order.opened · sales.order_line.added · sales.order_line.updated ·
sales.order_line.voided · sales.order_line.fired · sales.table.transferred ·
sales.table.merged · kitchen.ticket.bumped · kitchen.ticket.recalled ·
billing.bill.split · billing.bill.merged · billing.discount.applied ·
billing.comp.applied · billing.payment.captured · billing.tip.adjusted ·
billing.bill.settled · billing.bill.voided · billing.refund.issued ·
cash.shift.opened · cash.shift.closed · cash.drawer.opened ·
cash.drawer.paid_in · cash.drawer.paid_out ·
inventory.item.sold_out · inventory.item.restored ·
inventory.stock.consumed · inventory.stock.adjusted · inventory.stock.counted ·
promotion.voucher.redeemed · delivery.shipment.created · delivery.shipment.status_changed ·
config.version.published · device.activation.completed · fleet.update.rolled_out
```

**Envelope thống nhất** (mọi sự kiện, mọi kênh):
```json
{
  "event_id": "01J...",            "event_type": "billing.bill.settled",
  "event_time": "2026-08-14T03:12:45.123Z", "schema_version": 1,
  "tenant_id": "…", "brand_id": "…", "store_id": "…",
  "device_id": "…", "employee_id": "…", "shift_id": "…",
  "data": { … }
}
```
`event_id` là ULID và là **khóa idempotency** của bên nhận. `schema_version` chỉ tăng khi buộc phải phá vỡ — mặc định mọi thay đổi là cộng thêm.

## 6. Cơ sở dữ liệu (PostgreSQL & SQLite dùng chung quy ước)

| Đối tượng | Quy ước | Ví dụ |
|---|---|---|
| Bảng | số nhiều, snake_case | `orders`, `order_lines`, `bill_payments`, `stock_ledger_entries` |
| Cột | **trùng tên trường JSON** | `store_id`, `create_time`, `amount_minor` |
| Khóa chính | `<resource>_id` | `order_id` |
| Chỉ mục | `idx_<bảng>_<cột…>` | `idx_orders_store_id_create_time` |
| Duy nhất | `uq_<bảng>_<cột…>` | `uq_bills_store_id_receipt_number` |
| Khóa ngoại | `fk_<bảng>_<bảng_đích>` | `fk_order_lines_orders` |
| Ràng buộc kiểm tra | `ck_<bảng>_<luật>` | `ck_bill_payments_amount_minor_positive` |
| Phân vùng | `<bảng>_p_<khóa>` | `events_p_2026_08` |
| Enum | lưu `text` + `ck_` giới hạn giá trị, **trùng chuỗi trên dây** | `'ORDER_STATE_OPEN'` |

Cấm viết tắt kiểu `ord_ln`, cấm tiền tố `tbl_`, cấm cột `id` trần.

## 7. Cấu hình & capability flags

Khóa cấu hình theo đường dẫn snake_case, phản chiếu cây Tenant→Brand→Store:
`store.printing.default_printer_id` · `store.pos.tables_enabled` · `store.tax.tax_class_rates` · `brand.menu.daypart_schedules` · `tenant.integration.webhook_endpoints`.

## 8. Quyền RBAC

`domain.resource.action` — **cùng taxonomy với sự kiện**: `sales.order.create` · `sales.order_line.void_fired` · `billing.discount.over_limit` · `billing.bill.void` · `cash.drawer.open_standalone` · `inventory.stock.adjust` · `admin.role.manage`. Mã quyền là hợp đồng: chỉ thêm, không xóa.

## 9. Metrics & log

**Metrics** theo chuẩn Prometheus: `pos_<hệ>_<đối_tượng>_<đơn_vị>` — `pos_cloud_events_ingested_total`, `pos_edge_sync_lag_seconds`, `pos_cloud_webhook_delivery_duration_seconds`. Nhãn snake_case, **cấm nhãn có lực lượng cao** (không `order_id`): chỉ `store_id`, `adapter`, `event_type`, `status`.

**Log** dạng cấu trúc, tên trường trùng tên nghiệp vụ: `{"level":"error","event_type":"billing.payment.captured","store_id":"…","error_status":"UNAVAILABLE"}`. Cấm log PII (luật code hiện hành).

## 10. Mã nguồn

Crate: `kebab-case` (`pos-core`, `pos-ports`, `fiscal-vn`) — theo chuẩn Cargo. Module/file/hàm/biến: `snake_case`. Type/trait/enum type: `PascalCase`. Hằng: `SCREAMING_SNAKE_CASE`. Biến thể enum trong Rust là `PascalCase`, serialize ra dây thành `UPPER_SNAKE_CASE` bằng `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`.

## 11. Bảng đổi tên (áp cho tài liệu hiện có)

| Cũ | Mới |
|---|---|
| `OrderOpened`, `LineAdded`, `LineFired`, `KdsBumped` | `sales.order.opened`, `sales.order_line.added`, `sales.order_line.fired`, `kitchen.ticket.bumped` |
| `BillSettled`, `PaymentCaptured`, `DiscountApplied` | `billing.bill.settled`, `billing.payment.captured`, `billing.discount.applied` |
| `ShiftOpened/Closed`, `PaidInOut`, `DrawerOpened` | `cash.shift.opened/closed`, `cash.drawer.paid_in` / `paid_out`, `cash.drawer.opened` |
| `ItemSoldOut`, `ItemRestored` | `inventory.item.sold_out`, `inventory.item.restored` |
| `occurred_at`, `created_at` | `event_time`, `create_time` |
| `after=<ulid>` (Event Feed) | `page_token=<ulid>` + `page_size` |
| cờ `tables`, `tips`, `pay_first` | `tables_enabled`, `tips_enabled`, `pay_first_enabled` |

## 12. Bốn sai lệch có chủ đích so với Google AIP

| Sai lệch | Google | Mình | Lý do |
|---|---|---|---|
| JSON case | proto→JSON `lowerCamelCase` | `snake_case` | Không dùng proto-JSON mapping; một luật duy nhất cho JSON/DB/event/quyền quan trọng hơn việc giống hình thức |
| Collection ID trong URL | `lowerCamelCase` (AIP-122) | `snake_case` | Cùng lý do trên; va chạm hiếm vì hầu hết tài nguyên một từ |
| Tiền | `google.type.Money` (`units` + `nanos`) | `currency_code` + `amount_minor` | POS không cần độ chính xác dưới đơn vị tiền; `nanos` mời gọi tư duy số thực, trái luật tiền-số-nguyên |
| Định danh tài nguyên | trường `name` chứa đường dẫn | `<resource>_id` | Đơn giản, dự đoán được, khớp thẳng khóa chính DB; đường dẫn kiểu `name` quá nặng cho POS |

## 13. Thi hành bằng máy (không dựa vào trí nhớ)

Bổ sung vào CI (Kế hoạch GitHub mục 4 & 11):
1. **Linter đặt tên**: quét OpenAPI spec + migration SQL + registry sự kiện/quyền, chặn `camelCase`, `created_at`, cột `id` trần, tên có viết tắt ngoài danh sách trắng, enum thiếu `*_UNSPECIFIED`.
2. **Snapshot ba danh mục** (API public, schema sự kiện, danh mục quyền) — đổi tên = diff thấy được trong PR; xóa = cấm.
3. **Test đối chiếu tên**: kiểm tra tên cột DB ≡ tên trường JSON cho các bảng nghiệp vụ chính — chặn phân kỳ ngay khi mới sinh.
4. **OpenAPI sinh từ code**, không viết tay — tên trên tài liệu luôn bằng tên trên dây.

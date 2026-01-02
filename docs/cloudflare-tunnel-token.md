# Sử dụng Cloudflare Tunnel bằng token để hỗ trợ WebSocket

Tài liệu này giải thích lý do vì sao kết nối `wss://may1.hoadev.online/bridge` không thành công khi chỉ chạy `cloudflared` với `--token`, và cách cấu hình để cấp token cho user nhưng vẫn forward đúng tới dịch vụ WebSocket nội bộ (`http://127.0.0.1:15038`).

## Vì sao chạy bằng token bị lỗi

* Token được tạo từ Zero Trust chỉ chứa thông tin định danh tunnel/connector, **không mang ingress**. Nếu ingress không được lưu trên Cloudflare (hoặc để mặc định `http://localhost:80`), lưu lượng tới `may1.hoadev.online` sẽ bị gửi sai cổng.
* Dịch vụ backend của Tango Bridge dùng HTTP thường (không TLS) ở `127.0.0.1:15038`, nên nếu cấu hình trên Cloudflare để kết nối tới backend HTTPS thì bắt tay TLS sẽ lỗi và WSS không upgrade được.
* WebSocket cần header `Upgrade` được phép đi qua. Nếu ingress trên Cloudflare không chỉ định đúng dịch vụ hoặc hostname, Cloudflare sẽ trả lỗi 404/525 ngay trong bước bắt tay.

## Quy trình cấu hình đúng để cấp token cho user

1. **Tạo named tunnel và DNS** (chỉ làm một lần bằng tài khoản quản trị):
   ```bash
   cloudflared tunnel login
   cloudflared tunnel create may1
   cloudflared tunnel route dns may1 may1.hoadev.online
   ```
2. **Khai báo ingress (Public Hostname) trên Cloudflare Zero Trust** cho tunnel `may1`:
   * Hostname: `may1.hoadev.online`
   * Service: `http://127.0.0.1:15038`
   * Bật WebSocket (mặc định Cloudflare hỗ trợ; chỉ cần chọn HTTP/S service, không cần HTTPS tới backend).
   Lúc này ingress được lưu trên Cloudflare, không cần `config.yml` local.
3. **Cấp token cho user chạy connector**:
   ```bash
   cloudflared tunnel token may1
   ```
   Gửi token cho user và yêu cầu họ chạy:
   ```bash
   cloudflared tunnel --no-autoupdate --token <TOKEN_DA_CAP>
   ```
   Vì ingress đã nằm trên Cloudflare, connector dùng token sẽ tự nhận cấu hình và forward đúng tới `http://127.0.0.1:15038`.
4. **Kiểm tra**: từ trình duyệt tại `https://app.hoadev.online`, kết nối `wss://may1.hoadev.online/bridge` phải thành công; nếu không, kiểm tra log trên trang Tunnel của Cloudflare để xem có lỗi `origin` hay `bad gateway`.

## Lưu ý bảo mật

* Token chỉ cho phép chạy thêm connector, không chỉnh sửa cấu hình tunnel. Giữ `config.yml` và quyền truy cập Zero Trust UI cho admin.
* Nếu backend yêu cầu kiểm tra `Origin`, đảm bảo cho phép `https://app.hoadev.online` để tránh bị chặn khi đi qua domain khác.
* Nên bật nhật ký (`--loglevel info`) khi debug: `cloudflared tunnel --loglevel info --token <TOKEN_DA_CAP>`.

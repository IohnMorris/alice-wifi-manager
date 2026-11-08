pub const CAPTIVE_PORTAL_URL: &str = "http://192.168.4.1/";

/// Проверяет, является ли HTTP-запрос системной проверкой Captive Portal или запросом к внешнему хосту.
pub fn is_captive_probe(path: &str, host_header: Option<&str>) -> bool {
    let clean_path = path.split('?').next().unwrap_or(path);

    // Штатные проверочные пути iOS/macOS, Android и Windows
    if matches!(
        clean_path,
        "/hotspot-detect.html"
            | "/library/test/success.html"
            | "/generate_204"
            | "/gen_204"
            | "/connecttest.txt"
            | "/ncsi.txt"
            | "/redirect"
            | "/canonical.html"
            | "/success.txt"
    ) {
        return true;
    }

    // Если смартфон запрашивает произвольный домен (например, captive.apple.com или connectivitycheck.gstatic.com)
    if let Some(host) = host_header {
        let host_only = host.split(':').next().unwrap_or(host).trim();
        if !host_only.is_empty() && host_only != "192.168.4.1" && host_only != "localhost" {
            return true;
        }
    }

    false
}

/// Формирует HTTP-ответ `302 Found` с редиректом на локальную панель управления.
pub fn build_captive_redirect_response() -> Vec<u8> {
    let body = "<!DOCTYPE html><html><head><meta http-equiv=\"refresh\" content=\"0; url=http://192.168.4.1/\" /></head><body><a href=\"http://192.168.4.1/\">Open Alice WiFi Manager</a></body></html>";
    let header = format!(
        "HTTP/1.1 302 Found\r\n\
         Location: {}\r\n\
         Cache-Control: no-store, no-cache, must-revalidate, max-age=0\r\n\
         Pragma: no-cache\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        CAPTIVE_PORTAL_URL,
        body.len()
    );

    let mut resp = header.into_bytes();
    resp.extend_from_slice(body.as_bytes());
    resp
}

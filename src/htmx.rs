use axum::http::{HeaderName, HeaderValue};
use axum_extra::headers::Header;

pub struct HxRefresh(pub bool);

impl Header for HxRefresh {
    fn name() -> &'static axum::http::HeaderName {
        static NAME: HeaderName = HeaderName::from_static("hx-refresh");
        &NAME
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, axum_extra::headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i axum::http::HeaderValue>,
    {
        let mut refresh = false;
        for value in values {
            refresh = value == "true";
        }

        Ok(HxRefresh(refresh))
    }

    fn encode<E: Extend<axum::http::HeaderValue>>(&self, values: &mut E) {
        if self.0 {
            values.extend([HeaderValue::from_static("true")]);
        }
    }
}

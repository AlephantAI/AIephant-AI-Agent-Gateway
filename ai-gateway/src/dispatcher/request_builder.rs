use reqwest::RequestBuilder;

use crate::utils::host_header;

pub(super) fn request_builder_with_effective_host(
    request_builder: RequestBuilder,
    target_url: &url::Url,
) -> RequestBuilder {
    request_builder.header(http::header::HOST, host_header(target_url))
}

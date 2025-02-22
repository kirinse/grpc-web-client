mod call;
mod errors;

use bytes::Bytes;
use call::{Encoding, GrpcWebCall};
use core::task::{Context, Poll};
use errors::ClientError;
use futures::{Future, Stream, TryStreamExt};
use http::{header::HeaderName, request::Request, response::Response, HeaderMap, HeaderValue, Uri};
use http_body::Body;
use js_sys::{Array, Uint8Array};
use std::pin::Pin;
use tonic::{body::BoxBody, Status};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use wasm_streams::ReadableStream;
use web_sys::{Headers, RequestInit};
use tower_service::Service;

pub type CredentialsMode = web_sys::RequestCredentials;

pub type RequestMode = web_sys::RequestMode;

#[derive(Clone)]
pub struct Client {
    base_uri: String,
    credentials: CredentialsMode,
    mode: RequestMode,
    encoding: Encoding,
}

impl Client {
    // pub fn new(base_uri: String) -> Self {

    /// Create a new [Client] connecting to the given endpoint.
    ///
    /// The [Uri] should have a scheme, an authority and no query.
    pub fn new(uri: &Uri) -> Result<Self, ClientError> {
        uri.scheme()
            .and(uri.authority())
            .and(uri.path_and_query())
            .map(|path_and_query| path_and_query.query())
            .ok_or(ClientError::InvalidUri)?;

        let mut base_uri = uri.to_string();
        if uri.path() == "/" {
            base_uri.pop();
        }

        Ok(Client {
            base_uri,
            credentials: CredentialsMode::SameOrigin,
            mode: RequestMode::Cors,
            encoding: Encoding::None,
        })
    }

    async fn request(self, rpc: Request<BoxBody>) -> Result<Response<BoxBody>, ClientError> {
        // let mut uri = rpc.uri().to_string();
        // uri.insert_str(0, &self.base_uri);
        assert_eq!(rpc.uri().scheme(), None);
        assert_eq!(rpc.uri().authority(), None);
        let mut uri = self.base_uri.clone();
        uri.push_str(rpc.uri().path_and_query().unwrap().as_str());

        let headers = Headers::new()?;
        for (k, v) in rpc.headers().iter() {
            headers.set(k.as_str(), v.to_str()?)?;
        }
        headers.set("x-user-agent", "grpc-web-rust/0.1")?;
        headers.set("x-grpc-web", "1")?;
        headers.set("content-type", self.encoding.to_content_type())?;

        let body_bytes = hyper::body::to_bytes(rpc.into_body()).await?;
        let body_array: Uint8Array = body_bytes.as_ref().into();
        let body_js: &JsValue = body_array.as_ref();

        let mut init = RequestInit::new();
        init.method("POST")
            .mode(self.mode)
            .credentials(self.credentials)
            .body(Some(body_js))
            .headers(headers.as_ref());

        let request = web_sys::Request::new_with_str_and_init(&uri, &init)?;

        let window =
            web_sys::window().ok_or(ClientError::UnexpectedOptionNone("Could not get window"))?;
        let fetch = JsFuture::from(window.fetch_with_request(&request)).await?;
        let fetch_res: web_sys::Response = fetch.dyn_into()?;

        let mut res = Response::builder().status(fetch_res.status());
        let headers = res.headers_mut().ok_or(ClientError::UnexpectedOptionNone(
            "Could not get headers from response builder",
        ))?;

        for kv in js_sys::try_iter(fetch_res.headers().as_ref())?.ok_or(
            ClientError::UnexpectedOptionNone("Could not get headers from fetch response"),
        )? {
            let pair: Array = kv?.into();
            headers.append(
                HeaderName::from_bytes(
                    pair.get(0)
                        .as_string()
                        .ok_or(ClientError::UnexpectedOptionNone(
                            "Header name is not a string",
                        ))?
                        .as_bytes(),
                )?,
                HeaderValue::from_str(&pair.get(1).as_string().ok_or(
                    ClientError::UnexpectedOptionNone("Header value is not a string"),
                )?)?,
            );
        }

        let body_stream = ReadableStream::from_raw(
            fetch_res
                .body()
                .ok_or(ClientError::UnexpectedOptionNone(
                    "Fetch response has no body",
                ))?
                .unchecked_into(),
        );
        let body = GrpcWebCall::client_response(
            ReadableStreamBody::new(body_stream),
            Encoding::from_content_type(headers),
        );

        Ok(res.body(BoxBody::new(body))?)
    }
}

impl Service<Request<BoxBody>> for Client {
    type Response = Response<BoxBody>;
    type Error = ClientError;
    type Future = Pin<Box<dyn Future<Output=Result<Response<BoxBody>, ClientError>>>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<BoxBody>) -> Self::Future {
        Box::pin(self.clone().request(req))
    }
}

struct ReadableStreamBody {
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, Status>>>>,
}

impl ReadableStreamBody {
    fn new(inner: ReadableStream) -> Self {
        ReadableStreamBody {
            stream: Box::pin(
                inner
                    .into_stream()
                    .map_ok(|buf_js| {
                        let buffer = Uint8Array::new(&buf_js);
                        let mut bytes_vec = vec![0; buffer.length() as usize];
                        buffer.copy_to(&mut bytes_vec);
                        let bytes: Bytes = bytes_vec.into();
                        bytes
                    })
                    .map_err(|_| Status::unknown("readablestream error")),
            ),
        }
    }
}

impl Body for ReadableStreamBody {
    type Data = Bytes;
    type Error = Status;

    fn poll_data(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        self.stream.as_mut().poll_next(cx)
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Result<Option<HeaderMap>, Self::Error>> {
        Poll::Ready(Ok(None))
    }

    fn is_end_stream(&self) -> bool {
        false
    }
}

// WARNING: these are required to satisfy the Body and Error traits, but JsValue is not thread-safe.
// This shouldn't be an issue because wasm doesn't have threads currently.

unsafe impl Sync for ReadableStreamBody {}
unsafe impl Send for ReadableStreamBody {}

unsafe impl Sync for ClientError {}
unsafe impl Send for ClientError {}
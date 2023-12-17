// 定义一个提取器，主要作用是用来处理错误，将axum抛出的错误转为我们自定义的错误
use crate::errors::AppError;
use axum::{
    async_trait,
    extract::{rejection::JsonRejection, FromRequest, MatchedPath, Request},
    http::StatusCode,
    RequestPartsExt,
};

pub struct JsonExtractor<T>(pub T);

#[async_trait]
impl<S, T> FromRequest<S> for JsonExtractor<T>
where
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = (StatusCode, AppError);

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let (mut parts, body) = req.into_parts();

        // We can use other extractors to provide better rejection messages.
        // For example, here we are using `axum::extract::MatchedPath` to
        // provide a better error message.
        //
        // Have to run that first since `Json` extraction consumes the request.
        let path = parts
            .extract::<MatchedPath>()
            .await
            .map(|path| path.as_str().to_owned())
            .ok();

        let req = Request::from_parts(parts, body);

        match axum::Json::<T>::from_request(req, state).await {
            Ok(value) => Ok(Self(value.0)),
            // convert the error from `axum::Json` into whatever we want
            Err(rejection) => {
                let mut ph = String::new();
                if path.is_some() {
                    ph = path.unwrap();
                }
                let app_err = AppError::BodyParsing(rejection.body_text(), ph);

                Err((rejection.status(), app_err))
            }
        }
    }
}
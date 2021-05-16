use {
    crate::{
        asset::{Asset, AssetBuild},
        loader::{self, AssetHandle, AssetResult, Loader},
    },
    futures::future::TryJoinAll,
    std::{
        convert::Infallible,
        error::Error,
        future::Future,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    },
    uuid::Uuid,
};

pub enum External {}

pub enum Container {}

// pub enum Inline {}

pub trait AssetField<K>: Clone + Sized + Send + Sync + 'static {
    /// Deserializable information about asset field.
    type Info: serde::de::DeserializeOwned;

    /// Decoded representation of this asset.
    type Decoded: Send + Sync;

    /// Decoding error.
    type DecodeError: Error + Send + Sync + 'static;

    /// Building error.
    type BuildError: Error + Send + Sync + 'static;

    /// Future that will resolve into decoded asset when ready.
    type Fut: Future<Output = Result<Self::Decoded, Self::DecodeError>> + Send;

    fn decode(info: Self::Info, loader: &Loader) -> Self::Fut;
}

pub trait AssetFieldBuild<K, B>: AssetField<K> {
    /// Build asset instance using decoded representation and `Resources`.
    fn build(decoded: Self::Decoded, builder: &mut B) -> Result<Self, Self::BuildError>;
}

impl<A> AssetField<External> for A
where
    A: Asset,
{
    type Info = Uuid;
    type DecodeError = Infallible;
    type BuildError = loader::Error;
    type Decoded = AssetResult<A>;
    type Fut = ExternAssetFut<A>;

    fn decode(uuid: Uuid, loader: &Loader) -> Self::Fut {
        ExternAssetFut(loader.load(&uuid))
    }
}

impl<A, B> AssetFieldBuild<External, B> for A
where
    A: Asset + AssetBuild<B>,
{
    fn build(mut result: AssetResult<A>, builder: &mut B) -> Result<A, loader::Error> {
        result.get(builder).map(A::clone)
    }
}

pub struct ExternAssetFut<A>(AssetHandle<A>);

impl<A> Future for ExternAssetFut<A>
where
    A: Asset,
{
    type Output = Result<AssetResult<A>, Infallible>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        Pin::new(&mut me.0).poll(cx).map(Ok)
    }
}

impl<K, A> AssetField<K> for Option<A>
where
    A: AssetField<K>,
{
    type Info = Option<A::Info>;
    type DecodeError = A::DecodeError;
    type BuildError = A::BuildError;
    type Decoded = Option<A::Decoded>;
    type Fut = MaybeTryFuture<A::Fut>;

    fn decode(info: Option<A::Info>, loader: &Loader) -> Self::Fut {
        match info {
            None => MaybeTryFuture(None),
            Some(info) => MaybeTryFuture(Some(A::decode(info, loader))),
        }
    }
}

impl<K, B, A> AssetFieldBuild<K, B> for Option<A>
where
    A: AssetField<K> + AssetFieldBuild<K, B>,
{
    fn build(
        maybe_decoded: Option<A::Decoded>,
        builder: &mut B,
    ) -> Result<Option<A>, A::BuildError> {
        match maybe_decoded {
            Some(decoded) => A::build(decoded, builder).map(Some),
            None => Ok(None),
        }
    }
}

pub struct MaybeTryFuture<F>(Option<F>);

impl<F, R, E> Future for MaybeTryFuture<F>
where
    F: Future<Output = Result<R, E>>,
{
    type Output = Result<Option<R>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let maybe_fut = unsafe { self.map_unchecked_mut(|me| &mut me.0) }.as_pin_mut();

        match maybe_fut {
            None => Poll::Ready(Ok(None)),
            Some(fut) => match fut.poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(result) => Poll::Ready(result.map(Some)),
            },
        }
    }
}

impl<K, A> AssetField<K> for Arc<[A]>
where
    A: AssetField<K>,
{
    type Info = Vec<A::Info>;
    type DecodeError = A::DecodeError;
    type BuildError = A::BuildError;
    type Decoded = Vec<A::Decoded>;
    type Fut = TryJoinAll<A::Fut>;

    fn decode(info: Vec<A::Info>, loader: &Loader) -> Self::Fut {
        info.into_iter()
            .map(|info| A::decode(info, loader))
            .collect()
    }
}

impl<K, B, A> AssetFieldBuild<K, B> for Arc<[A]>
where
    A: AssetField<K> + AssetFieldBuild<K, B>,
{
    fn build(decoded: Vec<A::Decoded>, builder: &mut B) -> Result<Arc<[A]>, A::BuildError> {
        decoded
            .into_iter()
            .map(|decoded| A::build(decoded, builder))
            .collect()
    }
}

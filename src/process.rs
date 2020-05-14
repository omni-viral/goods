use {
    crate::{
        asset::Asset,
        handle::Handle,
        queue::Queue,
        sync::{Ptr, Send},
        Error,
    },
    alloc::{boxed::Box, vec::Vec},
    core::{any::Any, marker::PhantomData},
};

pub(crate) struct ProcessSlot<A: Asset> {
    handle: Handle<A>,
    queue: Ptr<Queue<Box<dyn AnyProcess<A::Context>>>>,
}

impl<A> ProcessSlot<A>
where
    A: Asset,
{
    pub(crate) fn set(self, result: Result<A::Repr, Error<A>>) {
        self.queue.push(Box::new(Process {
            result,
            handle: self.handle,
        }))
    }
}

pub(crate) trait AnyProcess<C>: Send {
    fn run(self: Box<Self>, ctx: &mut C);
}

struct Process<A: Asset> {
    handle: Handle<A>,
    result: Result<A::Repr, Error<A>>,
}

impl<A> Process<A> where A: Asset {}

impl<A> AnyProcess<A::Context> for Process<A>
where
    A: Asset,
{
    fn run(self: Box<Self>, ctx: &mut A::Context) {
        let result = self
            .result
            .and_then(|asset| A::build(asset, ctx).map_err(|err| Error::Asset(Ptr::new(err))));

        self.handle.set(result);
    }
}

struct Processes<C> {
    queue: Ptr<Queue<Box<dyn AnyProcess<C>>>>,
}

impl<C> Processes<C> {
    fn new() -> Self {
        Processes {
            queue: Ptr::new(Queue::new()),
        }
    }

    fn run(&mut self) -> Vec<Box<dyn AnyProcess<C>>> {
        let mut received = Vec::new();
        self.queue.take(&mut received);
        received
    }
}

pub(crate) struct AnyProcesses<K> {
    #[cfg(not(feature = "sync"))]
    inner: Box<dyn Any>,

    #[cfg(feature = "sync")]
    inner: Box<dyn Any + Send>,
    marker: PhantomData<fn(K)>,
}

impl<K> AnyProcesses<K>
where
    K: 'static,
{
    pub(crate) fn new<C: 'static>() -> Self {
        AnyProcesses {
            inner: Box::new(Processes::<C>::new()),
            marker: PhantomData,
        }
    }

    pub(crate) fn alloc<A>(&self) -> (Handle<A>, ProcessSlot<A>)
    where
        A: Asset,
    {
        let queue = Any::downcast_ref::<Processes<A::Context>>(&*self.inner)
            .unwrap()
            .queue
            .clone();
        let handle = Handle::new();
        let slot = ProcessSlot {
            handle: handle.clone(),
            queue,
        };
        (handle, slot)
    }

    pub(crate) fn run<C: 'static>(&mut self) -> Vec<Box<dyn AnyProcess<C>>> {
        Any::downcast_mut::<Processes<C>>(&mut *self.inner)
            .unwrap()
            .run()
    }
}

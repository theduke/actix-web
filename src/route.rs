use std::cell::{RefCell, UnsafeCell};
use std::marker::PhantomData;
use std::rc::Rc;

use futures::{Async, Future, Poll};

use error::Error;
use handler::{
    AsyncHandler, AsyncResult, AsyncResultItem, FromRequest, Handler, Responder,
    RouteHandler, WrapHandler,
};
use http::StatusCode;
use httprequest::HttpRequest;
use httpresponse::HttpResponse;
use middleware::{
    Finished as MiddlewareFinished, Middleware, Response as MiddlewareResponse,
    Started as MiddlewareStarted,
};
use pred::Predicate;
use with::{ExtractorConfig, With, WithAsync};

/// Resource route definition
///
/// Route uses builder-like pattern for configuration.
/// If handler is not explicitly set, default *404 Not Found* handler is used.
pub struct Route<S> {
    preds: Vec<Box<Predicate<S>>>,
    handler: InnerHandler<S>,
}

impl<S: 'static> Default for Route<S> {
    fn default() -> Route<S> {
        Route {
            preds: Vec::new(),
            handler: InnerHandler::new(|_| HttpResponse::new(StatusCode::NOT_FOUND)),
        }
    }
}

impl<S: 'static> Route<S> {
    #[inline]
    pub(crate) fn check(&self, req: &mut HttpRequest<S>) -> bool {
        for pred in &self.preds {
            if !pred.check(req) {
                return false;
            }
        }
        true
    }

    #[inline]
    pub(crate) fn handle(&mut self, req: HttpRequest<S>) -> AsyncResult<HttpResponse> {
        self.handler.handle(req)
    }

    #[inline]
    pub(crate) fn compose(
        &mut self, req: HttpRequest<S>, mws: Rc<RefCell<Vec<Box<Middleware<S>>>>>,
    ) -> AsyncResult<HttpResponse> {
        AsyncResult::async(Box::new(Compose::new(req, mws, self.handler.clone())))
    }

    /// Add match predicate to route.
    ///
    /// ```rust
    /// # extern crate actix_web;
    /// # use actix_web::*;
    /// # fn main() {
    /// App::new().resource("/path", |r| {
    ///     r.route()
    ///         .filter(pred::Get())
    ///         .filter(pred::Header("content-type", "text/plain"))
    ///         .f(|req| HttpResponse::Ok())
    /// })
    /// #      .finish();
    /// # }
    /// ```
    pub fn filter<T: Predicate<S> + 'static>(&mut self, p: T) -> &mut Self {
        self.preds.push(Box::new(p));
        self
    }

    /// Set handler object. Usually call to this method is last call
    /// during route configuration, so it does not return reference to self.
    pub fn h<H: Handler<S>>(&mut self, handler: H) {
        self.handler = InnerHandler::new(handler);
    }

    /// Set handler function. Usually call to this method is last call
    /// during route configuration, so it does not return reference to self.
    pub fn f<F, R>(&mut self, handler: F)
    where
        F: Fn(HttpRequest<S>) -> R + 'static,
        R: Responder + 'static,
    {
        self.handler = InnerHandler::new(handler);
    }

    /// Set async handler function.
    pub fn a<H, R, F, E>(&mut self, handler: H)
    where
        H: Fn(HttpRequest<S>) -> F + 'static,
        F: Future<Item = R, Error = E> + 'static,
        R: Responder + 'static,
        E: Into<Error> + 'static,
    {
        self.handler = InnerHandler::async(handler);
    }

    /// Set handler function, use request extractor for parameters.
    ///
    /// ```rust
    /// # extern crate bytes;
    /// # extern crate actix_web;
    /// # extern crate futures;
    /// #[macro_use] extern crate serde_derive;
    /// use actix_web::{http, App, Path, Result};
    ///
    /// #[derive(Deserialize)]
    /// struct Info {
    ///     username: String,
    /// }
    ///
    /// /// extract path info using serde
    /// fn index(info: Path<Info>) -> Result<String> {
    ///     Ok(format!("Welcome {}!", info.username))
    /// }
    ///
    /// fn main() {
    ///     let app = App::new().resource(
    ///         "/{username}/index.html", // <- define path parameters
    ///         |r| r.method(http::Method::GET).with(index),
    ///     ); // <- use `with` extractor
    /// }
    /// ```
    ///
    /// It is possible to use tuples for specifing multiple extractors for one
    /// handler function.
    ///
    /// ```rust
    /// # extern crate bytes;
    /// # extern crate actix_web;
    /// # extern crate futures;
    /// #[macro_use] extern crate serde_derive;
    /// # use std::collections::HashMap;
    /// use actix_web::{http, App, Json, Path, Query, Result};
    ///
    /// #[derive(Deserialize)]
    /// struct Info {
    ///     username: String,
    /// }
    ///
    /// /// extract path info using serde
    /// fn index(
    ///     info: (Path<Info>, Query<HashMap<String, String>>, Json<Info>),
    /// ) -> Result<String> {
    ///     Ok(format!("Welcome {}!", info.0.username))
    /// }
    ///
    /// fn main() {
    ///     let app = App::new().resource(
    ///         "/{username}/index.html", // <- define path parameters
    ///         |r| r.method(http::Method::GET).with(index),
    ///     ); // <- use `with` extractor
    /// }
    /// ```
    pub fn with<T, F, R>(&mut self, handler: F) -> ExtractorConfig<S, T>
    where
        F: Fn(T) -> R + 'static,
        R: Responder + 'static,
        T: FromRequest<S> + 'static,
    {
        let cfg = ExtractorConfig::default();
        self.h(With::new(handler, Clone::clone(&cfg)));
        cfg
    }

    /// Set async handler function, use request extractor for parameters.
    /// Also this method needs to be used if your handler function returns
    /// `impl Future<>`
    ///
    /// ```rust
    /// # extern crate bytes;
    /// # extern crate actix_web;
    /// # extern crate futures;
    /// #[macro_use] extern crate serde_derive;
    /// use actix_web::{http, App, Error, Path};
    /// use futures::Future;
    ///
    /// #[derive(Deserialize)]
    /// struct Info {
    ///     username: String,
    /// }
    ///
    /// /// extract path info using serde
    /// fn index(info: Path<Info>) -> Box<Future<Item = &'static str, Error = Error>> {
    ///     unimplemented!()
    /// }
    ///
    /// fn main() {
    ///     let app = App::new().resource(
    ///         "/{username}/index.html", // <- define path parameters
    ///         |r| r.method(http::Method::GET).with_async(index),
    ///     ); // <- use `with` extractor
    /// }
    /// ```
    pub fn with_async<T, F, R, I, E>(&mut self, handler: F) -> ExtractorConfig<S, T>
    where
        F: Fn(T) -> R + 'static,
        R: Future<Item = I, Error = E> + 'static,
        I: Responder + 'static,
        E: Into<Error> + 'static,
        T: FromRequest<S> + 'static,
    {
        let cfg = ExtractorConfig::default();
        self.h(WithAsync::new(handler, Clone::clone(&cfg)));
        cfg
    }
}

/// `RouteHandler` wrapper. This struct is required because it needs to be
/// shared for resource level middlewares.
struct InnerHandler<S>(Rc<UnsafeCell<Box<RouteHandler<S>>>>);

impl<S: 'static> InnerHandler<S> {
    #[inline]
    fn new<H: Handler<S>>(h: H) -> Self {
        InnerHandler(Rc::new(UnsafeCell::new(Box::new(WrapHandler::new(h)))))
    }

    #[inline]
    fn async<H, R, F, E>(h: H) -> Self
    where
        H: Fn(HttpRequest<S>) -> F + 'static,
        F: Future<Item = R, Error = E> + 'static,
        R: Responder + 'static,
        E: Into<Error> + 'static,
    {
        InnerHandler(Rc::new(UnsafeCell::new(Box::new(AsyncHandler::new(h)))))
    }

    #[inline]
    pub fn handle(&self, req: HttpRequest<S>) -> AsyncResult<HttpResponse> {
        // reason: handler is unique per thread, handler get called from async code only
        let h = unsafe { &mut *self.0.as_ref().get() };
        h.handle(req)
    }
}

impl<S> Clone for InnerHandler<S> {
    #[inline]
    fn clone(&self) -> Self {
        InnerHandler(Rc::clone(&self.0))
    }
}

/// Compose resource level middlewares with route handler.
struct Compose<S: 'static> {
    info: ComposeInfo<S>,
    state: ComposeState<S>,
}

struct ComposeInfo<S: 'static> {
    count: usize,
    req: HttpRequest<S>,
    mws: Rc<RefCell<Vec<Box<Middleware<S>>>>>,
    handler: InnerHandler<S>,
}

enum ComposeState<S: 'static> {
    Starting(StartMiddlewares<S>),
    Handler(WaitingResponse<S>),
    RunMiddlewares(RunMiddlewares<S>),
    Finishing(FinishingMiddlewares<S>),
    Completed(Response<S>),
}

impl<S: 'static> ComposeState<S> {
    fn poll(&mut self, info: &mut ComposeInfo<S>) -> Option<ComposeState<S>> {
        match *self {
            ComposeState::Starting(ref mut state) => state.poll(info),
            ComposeState::Handler(ref mut state) => state.poll(info),
            ComposeState::RunMiddlewares(ref mut state) => state.poll(info),
            ComposeState::Finishing(ref mut state) => state.poll(info),
            ComposeState::Completed(_) => None,
        }
    }
}

impl<S: 'static> Compose<S> {
    fn new(
        req: HttpRequest<S>, mws: Rc<RefCell<Vec<Box<Middleware<S>>>>>, handler: InnerHandler<S>,
    ) -> Self {
        let mut info = ComposeInfo {
            count: 0,
            req,
            mws,
            handler,
        };
        let state = StartMiddlewares::init(&mut info);

        Compose { state, info }
    }
}

impl<S> Future for Compose<S> {
    type Item = HttpResponse;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            if let ComposeState::Completed(ref mut resp) = self.state {
                let resp = resp.resp.take().unwrap();
                return Ok(Async::Ready(resp));
            }
            if let Some(state) = self.state.poll(&mut self.info) {
                self.state = state;
            } else {
                return Ok(Async::NotReady);
            }
        }
    }
}

/// Middlewares start executor
struct StartMiddlewares<S> {
    fut: Option<Fut>,
    _s: PhantomData<S>,
}

type Fut = Box<Future<Item = Option<HttpResponse>, Error = Error>>;

impl<S: 'static> StartMiddlewares<S> {
    fn init(info: &mut ComposeInfo<S>) -> ComposeState<S> {
        let len = info.mws.borrow().len();
        loop {
            if info.count == len {
                let reply = info.handler.handle(info.req.clone());
                return WaitingResponse::init(info, reply);
            } else {
                let state = info.mws.borrow_mut()[info.count].start(&mut info.req);
                match state {
                    Ok(MiddlewareStarted::Done) => info.count += 1,
                    Ok(MiddlewareStarted::Response(resp)) => {
                        return RunMiddlewares::init(info, resp)
                    }
                    Ok(MiddlewareStarted::Future(fut)) => {
                        return ComposeState::Starting(StartMiddlewares {
                            fut: Some(fut),
                            _s: PhantomData,
                        })
                    }
                    Err(err) => return FinishingMiddlewares::init(info, err.into()),
                }
            }
        }
    }

    fn poll(&mut self, info: &mut ComposeInfo<S>) -> Option<ComposeState<S>> {
        let len = info.mws.borrow().len();
        'outer: loop {
            match self.fut.as_mut().unwrap().poll() {
                Ok(Async::NotReady) => return None,
                Ok(Async::Ready(resp)) => {
                    info.count += 1;
                    if let Some(resp) = resp {
                        return Some(RunMiddlewares::init(info, resp));
                    }
                    loop {
                        if info.count == len {
                            let reply = info.handler.handle(info.req.clone());
                            return Some(WaitingResponse::init(info, reply));
                        } else {
                            let state = info.mws.borrow_mut()[info.count].start(&mut info.req);
                            match state {
                                Ok(MiddlewareStarted::Done) => info.count += 1,
                                Ok(MiddlewareStarted::Response(resp)) => {
                                    return Some(RunMiddlewares::init(info, resp));
                                }
                                Ok(MiddlewareStarted::Future(fut)) => {
                                    self.fut = Some(fut);
                                    continue 'outer;
                                }
                                Err(err) => {
                                    return Some(FinishingMiddlewares::init(
                                        info,
                                        err.into(),
                                    ))
                                }
                            }
                        }
                    }
                }
                Err(err) => return Some(FinishingMiddlewares::init(info, err.into())),
            }
        }
    }
}

// waiting for response
struct WaitingResponse<S> {
    fut: Box<Future<Item = HttpResponse, Error = Error>>,
    _s: PhantomData<S>,
}

impl<S: 'static> WaitingResponse<S> {
    #[inline]
    fn init(
        info: &mut ComposeInfo<S>, reply: AsyncResult<HttpResponse>,
    ) -> ComposeState<S> {
        match reply.into() {
            AsyncResultItem::Err(err) => RunMiddlewares::init(info, err.into()),
            AsyncResultItem::Ok(resp) => RunMiddlewares::init(info, resp),
            AsyncResultItem::Future(fut) => ComposeState::Handler(WaitingResponse {
                fut,
                _s: PhantomData,
            }),
        }
    }

    fn poll(&mut self, info: &mut ComposeInfo<S>) -> Option<ComposeState<S>> {
        match self.fut.poll() {
            Ok(Async::NotReady) => None,
            Ok(Async::Ready(response)) => Some(RunMiddlewares::init(info, response)),
            Err(err) => Some(RunMiddlewares::init(info, err.into())),
        }
    }
}

/// Middlewares response executor
struct RunMiddlewares<S> {
    curr: usize,
    fut: Option<Box<Future<Item = HttpResponse, Error = Error>>>,
    _s: PhantomData<S>,
}

impl<S: 'static> RunMiddlewares<S> {
    fn init(info: &mut ComposeInfo<S>, mut resp: HttpResponse) -> ComposeState<S> {
        let mut curr = 0;
        let len = info.mws.borrow().len();

        loop {
            let state = info.mws.borrow_mut()[curr].response(&mut info.req, resp);
            resp = match state {
                Err(err) => {
                    info.count = curr + 1;
                    return FinishingMiddlewares::init(info, err.into());
                }
                Ok(MiddlewareResponse::Done(r)) => {
                    curr += 1;
                    if curr == len {
                        return FinishingMiddlewares::init(info, r);
                    } else {
                        r
                    }
                }
                Ok(MiddlewareResponse::Future(fut)) => {
                    return ComposeState::RunMiddlewares(RunMiddlewares {
                        curr,
                        fut: Some(fut),
                        _s: PhantomData,
                    })
                }
            };
        }
    }

    fn poll(&mut self, info: &mut ComposeInfo<S>) -> Option<ComposeState<S>> {
        let len = info.mws.borrow().len();

        loop {
            // poll latest fut
            let mut resp = match self.fut.as_mut().unwrap().poll() {
                Ok(Async::NotReady) => return None,
                Ok(Async::Ready(resp)) => {
                    self.curr += 1;
                    resp
                }
                Err(err) => return Some(FinishingMiddlewares::init(info, err.into())),
            };

            loop {
                if self.curr == len {
                    return Some(FinishingMiddlewares::init(info, resp));
                } else {
                    let state = info.mws.borrow_mut()[self.curr].response(&mut info.req, resp);
                    match state {
                        Err(err) => {
                            return Some(FinishingMiddlewares::init(info, err.into()))
                        }
                        Ok(MiddlewareResponse::Done(r)) => {
                            self.curr += 1;
                            resp = r
                        }
                        Ok(MiddlewareResponse::Future(fut)) => {
                            self.fut = Some(fut);
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// Middlewares start executor
struct FinishingMiddlewares<S> {
    resp: Option<HttpResponse>,
    fut: Option<Box<Future<Item = (), Error = Error>>>,
    _s: PhantomData<S>,
}

impl<S: 'static> FinishingMiddlewares<S> {
    fn init(info: &mut ComposeInfo<S>, resp: HttpResponse) -> ComposeState<S> {
        if info.count == 0 {
            Response::init(resp)
        } else {
            let mut state = FinishingMiddlewares {
                resp: Some(resp),
                fut: None,
                _s: PhantomData,
            };
            if let Some(st) = state.poll(info) {
                st
            } else {
                ComposeState::Finishing(state)
            }
        }
    }

    fn poll(&mut self, info: &mut ComposeInfo<S>) -> Option<ComposeState<S>> {
        loop {
            // poll latest fut
            let not_ready = if let Some(ref mut fut) = self.fut {
                match fut.poll() {
                    Ok(Async::NotReady) => true,
                    Ok(Async::Ready(())) => false,
                    Err(err) => {
                        error!("Middleware finish error: {}", err);
                        false
                    }
                }
            } else {
                false
            };
            if not_ready {
                return None;
            }
            self.fut = None;
            if info.count == 0 {
                return Some(Response::init(self.resp.take().unwrap()));
            }

            info.count -= 1;

            let state = info.mws.borrow_mut()[info.count as usize]
                .finish(&mut info.req, self.resp.as_ref().unwrap());
            match state {
                MiddlewareFinished::Done => {
                    if info.count == 0 {
                        return Some(Response::init(self.resp.take().unwrap()));
                    }
                }
                MiddlewareFinished::Future(fut) => {
                    self.fut = Some(fut);
                }
            }
        }
    }
}

struct Response<S> {
    resp: Option<HttpResponse>,
    _s: PhantomData<S>,
}

impl<S: 'static> Response<S> {
    fn init(resp: HttpResponse) -> ComposeState<S> {
        ComposeState::Completed(Response {
            resp: Some(resp),
            _s: PhantomData,
        })
    }
}

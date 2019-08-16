use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use actix_http::{Error, Extensions};
use actix_service::boxed::{self};
use actix_service::{
    apply_transform, IntoNewService, IntoTransform, NewService, Transform,
};

use futures::{IntoFuture};
use regex::Regex;

use crate::data::Data;
use crate::dev::{insert_slash, AppService, HttpServiceFactory, ResourceDef};
use crate::extract::FromRequest;
use crate::guard::Guard;
use crate::handler::{AsyncFactory, Factory};
use crate::responder::Responder;
use crate::route::{Route};
use crate::service::{ServiceRequest, ServiceResponse};
use crate::resource::{
    CreateResourceService,
    HttpNewService,
    ResourceService,
    ResourceFactory
};

pub struct NormalizedResource<T = ResourceEndpoint> {
    endpoint: T,
    rdef: String,
    name: Option<String>,
    routes: Vec<Route>,
    data: Option<Extensions>,
    guards: Vec<Box<dyn Guard>>,
    default: Rc<RefCell<Option<Rc<HttpNewService>>>>,
    factory_ref: Rc<RefCell<Option<ResourceFactory>>>,
    merge_slash: Regex
}

impl NormalizedResource {
    pub fn new(path: &str) -> NormalizedResource {
        let fref = Rc::new(RefCell::new(None));

        NormalizedResource {
            routes: Vec::new(),
            rdef: path.to_string(),
            name: None,
            endpoint: ResourceEndpoint::new(fref.clone()),
            factory_ref: fref,
            guards: Vec::new(),
            data: None,
            default: Rc::new(RefCell::new(None)),
            merge_slash: Regex::new("//+").unwrap(),
        }
    }
}

impl<T> NormalizedResource<T>
where
    T: NewService<
        Config = (),
        Request = ServiceRequest,
        Response = ServiceResponse,
        Error = Error,
        InitError = (),
    >,
{
    /// Set resource name.
    ///
    /// Name is used for url generation.
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Add match guard to a resource.
    ///
    /// ```rust
    /// use actix_web::{web, guard, App, HttpResponse};
    ///
    /// fn index(data: web::Path<(String, String)>) -> &'static str {
    ///     "Welcome!"
    /// }
    ///
    /// fn main() {
    ///     let app = App::new()
    ///         .service(
    ///             web::normalized_resource("/app")
    ///                 .guard(guard::Header("content-type", "text/plain"))
    ///                 .route(web::get().to(index))
    ///         )
    ///         .service(
    ///             web::normalized_resource("/app")
    ///                 .guard(guard::Header("content-type", "text/json"))
    ///                 .route(web::get().to(|| HttpResponse::MethodNotAllowed()))
    ///         );
    /// }
    /// ```
    pub fn guard<G: Guard + 'static>(mut self, guard: G) -> Self {
        self.guards.push(Box::new(guard));
        self
    }

    pub(crate) fn add_guards(mut self, guards: Vec<Box<dyn Guard>>) -> Self {
        self.guards.extend(guards);
        self
    }

    /// Register a new route.
    ///
    /// ```rust
    /// use actix_web::{web, guard, App, HttpResponse};
    ///
    /// fn main() {
    ///     let app = App::new().service(
    ///         web::normalized_resource("/").route(
    ///             web::route()
    ///                 .guard(guard::Any(guard::Get()).or(guard::Put()))
    ///                 .guard(guard::Header("Content-Type", "text/plain"))
    ///                 .to(|| HttpResponse::Ok()))
    ///     );
    /// }
    /// ```
    ///
    /// Multiple routes could be added to a resource. Resource object uses
    /// match guards for route selection.
    ///
    /// ```rust
    /// use actix_web::{web, guard, App, HttpResponse};
    ///
    /// fn main() {
    ///     let app = App::new().service(
    ///         web::normalized_resource("/container/")
    ///              .route(web::get().to(get_handler))
    ///              .route(web::post().to(post_handler))
    ///              .route(web::delete().to(delete_handler))
    ///     );
    /// }
    /// # fn get_handler() {}
    /// # fn post_handler() {}
    /// # fn delete_handler() {}
    /// ```
    pub fn route(mut self, route: Route) -> Self {
        self.routes.push(route);
        self
    }

    /// Provide resource specific data. This method allows to add extractor
    /// configuration or specific state available via `Data<T>` extractor.
    /// Provided data is available for all routes registered for the current resource.
    /// Resource data overrides data registered by `App::data()` method.
    ///
    /// ```rust
    /// use actix_web::{web, App, FromRequest};
    ///
    /// /// extract text data from request
    /// fn index(body: String) -> String {
    ///     format!("Body {}!", body)
    /// }
    ///
    /// fn main() {
    ///     let app = App::new().service(
    ///         web::normalized_resource("/index.html")
    ///           // limit size of the payload
    ///           .data(String::configure(|cfg| {
    ///                cfg.limit(4096)
    ///           }))
    ///           .route(
    ///               web::get()
    ///                  // register handler
    ///                  .to(index)
    ///           ));
    /// }
    /// ```
    pub fn data<U: 'static>(mut self, data: U) -> Self {
        if self.data.is_none() {
            self.data = Some(Extensions::new());
        }
        self.data.as_mut().unwrap().insert(Data::new(data));
        self
    }

    /// Register a new route and add handler. This route matches all requests.
    ///
    /// ```rust
    /// use actix_web::*;
    ///
    /// fn index(req: HttpRequest) -> HttpResponse {
    ///     unimplemented!()
    /// }
    ///
    /// App::new().service(web::normalized_resource("/").to(index));
    /// ```
    ///
    /// This is shortcut for:
    ///
    /// ```rust
    /// # extern crate actix_web;
    /// # use actix_web::*;
    /// # fn index(req: HttpRequest) -> HttpResponse { unimplemented!() }
    /// App::new().service(web::normalized_resource("/").route(web::route().to(index)));
    /// ```
    pub fn to<F, I, R>(mut self, handler: F) -> Self
    where
        F: Factory<I, R> + 'static,
        I: FromRequest + 'static,
        R: Responder + 'static,
    {
        self.routes.push(Route::new().to(handler));
        self
    }

    /// Register a new route and add async handler.
    ///
    /// ```rust
    /// use actix_web::*;
    /// use futures::future::{ok, Future};
    ///
    /// fn index(req: HttpRequest) -> impl Future<Item=HttpResponse, Error=Error> {
    ///     ok(HttpResponse::Ok().finish())
    /// }
    ///
    /// App::new().service(web::normalized_resource("/").to_async(index));
    /// ```
    ///
    /// This is shortcut for:
    ///
    /// ```rust
    /// # use actix_web::*;
    /// # use futures::future::Future;
    /// # fn index(req: HttpRequest) -> Box<dyn Future<Item=HttpResponse, Error=Error>> {
    /// #     unimplemented!()
    /// # }
    /// App::new().service(web::normalized_resource("/").route(web::route().to_async(index)));
    /// ```
    #[allow(clippy::wrong_self_convention)]
    pub fn to_async<F, I, R>(mut self, handler: F) -> Self
    where
        F: AsyncFactory<I, R>,
        I: FromRequest + 'static,
        R: IntoFuture + 'static,
        R::Item: Responder,
        R::Error: Into<Error>,
    {
        self.routes.push(Route::new().to_async(handler));
        self
    }

    /// Register a resource middleware.
    ///
    /// This is similar to `App's` middlewares, but middleware get invoked on resource level.
    /// Resource level middlewares are not allowed to change response
    /// type (i.e modify response's body).
    ///
    /// **Note**: middlewares get called in opposite order of middlewares registration.
    pub fn wrap<M, F>(
        self,
        mw: F,
    ) -> NormalizedResource<
        impl NewService<
            Config = (),
            Request = ServiceRequest,
            Response = ServiceResponse,
            Error = Error,
            InitError = (),
        >,
    >
    where
        M: Transform<
            T::Service,
            Request = ServiceRequest,
            Response = ServiceResponse,
            Error = Error,
            InitError = (),
        >,
        F: IntoTransform<M, T::Service>,
    {
        let endpoint = apply_transform(mw, self.endpoint);
        NormalizedResource {
            endpoint,
            rdef: self.rdef,
            name: self.name,
            guards: self.guards,
            routes: self.routes,
            default: self.default,
            data: self.data,
            factory_ref: self.factory_ref,
            merge_slash: self.merge_slash,
        }
    }

    /// Register a resource middleware function.
    ///
    /// This function accepts instance of `ServiceRequest` type and
    /// mutable reference to the next middleware in chain.
    ///
    /// This is similar to `App's` middlewares, but middleware get invoked on resource level.
    /// Resource level middlewares are not allowed to change response
    /// type (i.e modify response's body).
    ///
    /// ```rust
    /// use actix_service::Service;
    /// # use futures::Future;
    /// use actix_web::{web, App};
    /// use actix_web::http::{header::CONTENT_TYPE, HeaderValue};
    ///
    /// fn index() -> &'static str {
    ///     "Welcome!"
    /// }
    ///
    /// fn main() {
    ///     let app = App::new().service(
    ///         web::normalized_resource("/index.html")
    ///             .wrap_fn(|req, srv|
    ///                 srv.call(req).map(|mut res| {
    ///                     res.headers_mut().insert(
    ///                        CONTENT_TYPE, HeaderValue::from_static("text/plain"),
    ///                     );
    ///                     res
    ///                 }))
    ///             .route(web::get().to(index)));
    /// }
    /// ```
    pub fn wrap_fn<F, R>(
        self,
        mw: F,
    ) -> NormalizedResource<
        impl NewService<
            Config = (),
            Request = ServiceRequest,
            Response = ServiceResponse,
            Error = Error,
            InitError = (),
        >,
    >
    where
        F: FnMut(ServiceRequest, &mut T::Service) -> R + Clone,
        R: IntoFuture<Item = ServiceResponse, Error = Error>,
    {
        self.wrap(mw)
    }

    /// Default service to be used if no matching route could be found.
    /// By default *405* response get returned. Resource does not use
    /// default handler from `App` or `Scope`.
    pub fn default_service<F, U>(mut self, f: F) -> Self
    where
        F: IntoNewService<U>,
        U: NewService<
                Config = (),
                Request = ServiceRequest,
                Response = ServiceResponse,
                Error = Error,
            > + 'static,
        U::InitError: fmt::Debug,
    {
        // create and configure default resource
        self.default = Rc::new(RefCell::new(Some(Rc::new(boxed::new_service(
            f.into_new_service().map_init_err(|e| {
                log::error!("Can not construct default service: {:?}", e)
            }),
        )))));

        self
    }
}

impl<T> HttpServiceFactory for NormalizedResource<T>
where
    T: NewService<
            Config = (),
            Request = ServiceRequest,
            Response = ServiceResponse,
            Error = Error,
            InitError = (),
        > + 'static,
{
    fn register(mut self, config: &mut AppService) {
        let guards_are_empty = self.guards.is_empty();
        let guards = if guards_are_empty {
            None
        } else {
            Some(std::mem::replace(&mut self.guards, Vec::new()))
        };
        let mut rdef = if config.is_root() || !self.rdef.is_empty() {
            ResourceDef::new(&insert_slash(&self.rdef))
        } else {
            ResourceDef::new(&self.rdef)
        };
        if let Some(ref name) = self.name {
            *rdef.name_mut() = name.clone();
        }
        // custom app data storage
        if let Some(ref mut ext) = self.data {
            config.set_service_data(ext);
        }

        
        let (guards1, guards2) = if guards_are_empty {
            (None, None)
        } else {
            let guards_rc = Rc::new(guards.unwrap());
            let guards_ref1: Option<Vec<Box<Guard + 'static>>> = Some(vec![Box::new(guards_rc.clone())]);
            let guards_ref2: Option<Vec<Box<Guard + 'static>>> = Some(vec![Box::new(guards_rc.clone())]);
            (guards_ref1, guards_ref2)
        };


        let cleaned_path = self.merge_slash.replace_all(rdef.pattern(), "/");

         let secondary_rdef = if cleaned_path.ends_with("/") {
             ResourceDef::new(&cleaned_path.trim_end_matches("/"))
         } else {
             let path_with_slash: String = format!("{}/", &cleaned_path);
             ResourceDef::new(&path_with_slash)
         };

        let service_rc = Rc::new(self.into_new_service());
        config.register_service(rdef, guards1, service_rc.clone(), None);
        config.register_service(secondary_rdef, guards2, service_rc.clone(), None);
    }
}

impl<T> IntoNewService<T> for NormalizedResource<T>
where
    T: NewService<
        Config = (),
        Request = ServiceRequest,
        Response = ServiceResponse,
        Error = Error,
        InitError = (),
    >,
{
    fn into_new_service(self) -> T {
        *self.factory_ref.borrow_mut() = Some(ResourceFactory {
            routes: self.routes,
            data: self.data.map(Rc::new),
            default: self.default,
        });

        self.endpoint
    }
}



#[doc(hidden)]
pub struct ResourceEndpoint {
    factory: Rc<RefCell<Option<ResourceFactory>>>,
}

impl ResourceEndpoint {
    fn new(factory: Rc<RefCell<Option<ResourceFactory>>>) -> Self {
        ResourceEndpoint { factory }
    }
}

impl NewService for ResourceEndpoint {
    type Config = ();
    type Request = ServiceRequest;
    type Response = ServiceResponse;
    type Error = Error;
    type InitError = ();
    type Service = ResourceService;
    type Future = CreateResourceService;

    fn new_service(&self, _: &()) -> Self::Future {
        self.factory.borrow_mut().as_mut().unwrap().new_service(&())
    }
}


#[cfg(test)]
mod tests {
    use std::time::Duration;

    use actix_service::Service;
    use futures::{Future, IntoFuture};
    use tokio_timer::sleep;

    use crate::http::{header, HeaderValue, Method, StatusCode};
    use crate::service::{ServiceRequest, ServiceResponse};
    use crate::test::{call_service, init_service, TestRequest};
    use crate::{guard, web, App, Error, HttpResponse};

    fn md<S, B>(
        req: ServiceRequest,
        srv: &mut S,
    ) -> impl IntoFuture<Item = ServiceResponse<B>, Error = Error>
    where
        S: Service<
            Request = ServiceRequest,
            Response = ServiceResponse<B>,
            Error = Error,
        >,
    {
        srv.call(req).map(|mut res| {
            res.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("0001"));
            res
        })
    }

    #[test]
    fn test_middleware() {
        let mut srv = init_service(
            App::new().service(
                web::normalized_resource("/test")
                    .name("test")
                    .wrap(md)
                    .route(web::get().to(|| HttpResponse::Ok())),
            ),
        );
        let req = TestRequest::with_uri("/test").to_request();
        let resp = call_service(&mut srv, req);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            HeaderValue::from_static("0001")
        );
    }

    #[test]
    fn test_middleware_fn() {
        let mut srv = init_service(
            App::new().service(
                web::normalized_resource("/test")
                    .wrap_fn(|req, srv| {
                        srv.call(req).map(|mut res| {
                            res.headers_mut().insert(
                                header::CONTENT_TYPE,
                                HeaderValue::from_static("0001"),
                            );
                            res
                        })
                    })
                    .route(web::get().to(|| HttpResponse::Ok())),
            ),
        );
        let req = TestRequest::with_uri("/test").to_request();
        let resp = call_service(&mut srv, req);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            HeaderValue::from_static("0001")
        );
    }

    #[test]
    fn test_to_async() {
        let mut srv =
            init_service(App::new().service(web::normalized_resource("/test").to_async(|| {
                sleep(Duration::from_millis(100)).then(|_| HttpResponse::Ok())
            })));
        let req = TestRequest::with_uri("/test").to_request();
        let resp = call_service(&mut srv, req);
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn test_default_resource() {
        let mut srv = init_service(
            App::new()
                .service(
                    web::normalized_resource("/test").route(web::get().to(|| HttpResponse::Ok())),
                )
                .default_service(|r: ServiceRequest| {
                    r.into_response(HttpResponse::BadRequest())
                }),
        );
        let req = TestRequest::with_uri("/test").to_request();
        let resp = call_service(&mut srv, req);
        assert_eq!(resp.status(), StatusCode::OK);

        let req = TestRequest::with_uri("/test")
            .method(Method::POST)
            .to_request();
        let resp = call_service(&mut srv, req);
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);

        let mut srv = init_service(
            App::new().service(
                web::normalized_resource("/test")
                    .route(web::get().to(|| HttpResponse::Ok()))
                    .default_service(|r: ServiceRequest| {
                        r.into_response(HttpResponse::BadRequest())
                    }),
            ),
        );

        let req = TestRequest::with_uri("/test").to_request();
        let resp = call_service(&mut srv, req);
        assert_eq!(resp.status(), StatusCode::OK);

        let req = TestRequest::with_uri("/test")
            .method(Method::POST)
            .to_request();
        let resp = call_service(&mut srv, req);
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_resource_guards() {
        let mut srv = init_service(
            App::new()
                .service(
                    web::normalized_resource("/test/{p}")
                        .guard(guard::Get())
                        .to(|| HttpResponse::Ok()),
                )
                .service(
                    web::normalized_resource("/test/{p}")
                        .guard(guard::Put())
                        .to(|| HttpResponse::Created()),
                )
                .service(
                    web::normalized_resource("/test/{p}")
                        .guard(guard::Delete())
                        .to(|| HttpResponse::NoContent()),
                ),
        );

        let req = TestRequest::with_uri("/test/it")
            .method(Method::GET)
            .to_request();
        let resp = call_service(&mut srv, req);
        assert_eq!(resp.status(), StatusCode::OK);

         let req = TestRequest::with_uri("/test/it")
             .method(Method::PUT)
             .to_request();
         let resp = call_service(&mut srv, req);
         assert_eq!(resp.status(), StatusCode::CREATED);

         let req = TestRequest::with_uri("/test/it")
             .method(Method::DELETE)
             .to_request();
         let resp = call_service(&mut srv, req);
         assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

}

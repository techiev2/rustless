use serialize::json;

use valico;

use server::method;
use server::mime;
use backend;
use errors;
use framework;
use framework::path;

pub type EndpointHandler = Box<for<'a> Fn(framework::Client<'a>, &json::Object) -> backend::HandleResult<framework::Client<'a>> + 'static + Sync>;

#[allow(missing_copy_implementations)]
pub enum EndpointHandlerPresent {
    HandlerPresent
}

pub type EndpointBuilder = FnOnce(&mut Endpoint) -> EndpointHandlerPresent + 'static;

pub struct Endpoint {
    pub method: method::Method,
    pub path: path::Path,
    pub summary: Option<String>,
    pub desc: Option<String>,
    pub coercer: Option<valico::Builder>,
    pub consumes: Option<Vec<mime::Mime>>,
    pub produces: Option<Vec<mime::Mime>>,
    handler: Option<EndpointHandler>,
}

unsafe impl Send for Endpoint {}

impl Endpoint {

    pub fn new(method: method::Method, path: &str) -> Endpoint {
        Endpoint {
            method: method,
            path: path::Path::parse(path, true).unwrap(),
            summary: None,
            desc: None,
            coercer: None,
            consumes: None,
            produces: None,
            handler: None,
        }
    }

    pub fn build<F>(method: method::Method, path: &str, builder: F) -> Endpoint 
    where F: FnOnce(&mut Endpoint) -> EndpointHandlerPresent {
        let mut endpoint = Endpoint::new(method, path);
        builder(&mut endpoint);

        endpoint
    }

    pub fn summary(&mut self, summary: &str) {
        self.summary = Some(summary.to_string());
    }

    pub fn desc(&mut self, desc: &str) {
        self.desc = Some(desc.to_string());
    }

    pub fn consumes(&mut self, mimes: Vec<mime::Mime>) {
        self.consumes = Some(mimes);
    }

    pub fn produces(&mut self, mimes: Vec<mime::Mime>) {
        self.produces = Some(mimes);
    }

    pub fn params<F>(&mut self, builder: F) where F: FnOnce(&mut valico::Builder) + 'static {
        self.coercer = Some(valico::Builder::build(builder));
    }

    pub fn handle<F>(&mut self, handler: F) -> EndpointHandlerPresent
    where F: for<'a> Fn(framework::Client<'a>, &json::Object) -> backend::HandleResult<framework::Client<'a>> + Sync+Send {
        self.handler = Some(Box::new(handler));
        EndpointHandlerPresent::HandlerPresent
    }

    pub fn handle_boxed(&mut self, handler: EndpointHandler) -> EndpointHandlerPresent {
        self.handler = Some(handler);
        EndpointHandlerPresent::HandlerPresent
    }

    fn validate(&self, params: &mut json::Object) -> backend::HandleResult<()> {
        // Validate namespace params with valico
        if self.coercer.is_some() {
            // validate and coerce params
            let coercer = self.coercer.as_ref().unwrap();
            match coercer.process(params) {
                Ok(()) => Ok(()),
                Err(err) => return Err(Box::new(errors::Validation{ reason: err }) as Box<errors::Error>)
            }   
        } else {
            Ok(())
        }
    }

    pub fn call_decode<'a>(&self, params: &mut json::Object, req: &'a mut (backend::Request + 'a), 
                       info: &mut framework::CallInfo) -> backend::HandleResult<backend::Response> {
        
        let mut client = framework::Client::new(info.app, self, req, &info.media);

        for parent in info.parents.iter() {
            try!(Endpoint::call_callbacks(parent.get_before(), &mut client, params));
        }

        for parent in info.parents.iter() {
            try!(Endpoint::call_callbacks(parent.get_before_validation(), &mut client, params));
        }

        try!(self.validate(params));

        for parent in info.parents.iter() {
            try!(Endpoint::call_callbacks(parent.get_after_validation(), &mut client, params));
        }

        let handler = self.handler.as_ref();
        let mut client = try!((handler.unwrap())(client, params));

        for parent in info.parents.iter() {
            try!(Endpoint::call_callbacks(parent.get_after(), &mut client, params));
        }

        Ok(client.move_response())
    }

    fn call_callbacks(cbs: &Vec<framework::Callback>, client: &mut framework::Client, params: &mut json::Object) 
    -> backend::HandleSuccessResult {
        for cb in cbs.iter() {
            try!(cb(client, params));
        }

        Ok(())
    }

}

impl framework::ApiHandler for Endpoint {
    fn api_call<'r>(&self, 
        rest_path: &str, 
        params: &mut json::Object, 
        req: &'r mut (backend::Request + 'r), 
        info: &mut framework::CallInfo) -> backend::HandleResult<backend::Response> {

        // method::Method guard
        if req.method() != &self.method {
            return Err(Box::new(errors::NotMatch) as Box<errors::Error>)
        }

        match self.path.is_match(rest_path) {
            Some(captures) =>  {
                self.path.apply_captures(params, captures);
                self.call_decode(params, req, info)
            },
            None => Err(Box::new(errors::NotMatch) as Box<errors::Error>)
        }

    }
}
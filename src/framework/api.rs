use collections;
use serialize::json;

use queryst;

use backend;
use server::mime;
use server::header;
use errors::{self, Error};

use framework::{self, ApiHandler};
use framework::app;
use framework::nesting::{self, Nesting, Node};
use framework::media;
use framework::formatters;
use framework::path;

#[allow(dead_code)]
#[allow(missing_copy_implementations)]
#[derive(Clone)]
pub enum Versioning {
    Path,
    AcceptHeader(&'static str),
    Param(&'static str)
}

#[derive(Clone)]
pub struct Version {
    pub version: String,
    pub versioning: Versioning,
}

pub struct Api {
    pub version: Option<Version>,
    pub prefix: Option<String>,
    pub handlers: framework::ApiHandlers,
    before: framework::Callbacks,
    before_validation: framework::Callbacks,
    after_validation: framework::Callbacks,
    after: framework::Callbacks,
    error_formatters: framework::ErrorFormatters,
    default_error_formatters: framework::ErrorFormatters,
    consumes: Option<Vec<mime::Mime>>,
    produces: Option<Vec<mime::Mime>>,
}

unsafe impl Send for Api {}

impl Api {

    pub fn new() -> Api {
        Api {
            version: None,
            prefix: None,
            handlers: vec![],
            before: vec![],
            before_validation: vec![],
            after_validation: vec![],
            after: vec![],
            error_formatters: vec![],
            default_error_formatters: vec![formatters::create_default_error_formatter()],
            consumes: None,
            produces: None,
        }
    }

    pub fn build<F>(builder: F) -> Api where F: FnOnce(&mut Api) {
        let mut api = Api::new();
        builder(&mut api);

        return api;
    }

    pub fn version(&mut self, version: &str, versioning: Versioning) {
        self.version = Some(Version {
            version: version.to_string(),
            versioning: versioning
        });
    }

    pub fn prefix(&mut self, prefix: &str) {
        self.prefix = Some(prefix.to_string());
    }

    pub fn consumes(&mut self, mimes: Vec<mime::Mime>) {
        self.consumes = Some(mimes);
    }

    pub fn produces(&mut self, mimes: Vec<mime::Mime>) {
        self.produces = Some(mimes);
    }

    pub fn error_formatter<F>(&mut self, formatter: F) 
    where F: Fn(&Box<Error + 'static>, &media::Media) -> Option<backend::Response> + Send+Sync {
        self.error_formatters.push(Box::new(formatter));
    }

    fn handle_error(&self, err: &Box<Error>, media: &media::Media) -> backend::Response  {
        for err_formatter in self.error_formatters.iter() {
            match err_formatter(err, media) {
                Some(resp) => return resp,
                None => ()
            }
        }

        for err_formatter in self.default_error_formatters.iter() {
            match err_formatter(err, media) {
                Some(resp) => return resp,
                None => ()
            }
        }

        unreachable!()
        
    }

    fn extract_media(&self, req: &backend::Request) -> Option<media::Media> {
        let header = req.headers().get::<header::Accept>();
        match header {
            Some(&header::Accept(ref mimes)) if !mimes.is_empty() => {
                // TODO: Allow only several mime types
                Some(media::Media::from_mime(&mimes[0].item))
            },
            _ => Some(media::Media::default())
        }
    }

    fn parse_query(query_str: &str, params: &mut json::Object) -> backend::HandleSuccessResult {
        let maybe_query_params = queryst::parse(query_str);
        match maybe_query_params {
            Ok(query_params) => {
                for (key, value) in query_params.as_object().unwrap().iter() {
                    if !params.contains_key(key) {
                        params.insert(key.to_string(), value.clone());
                    }
                }
            }, 
            Err(_) => {
                return Err(Box::new(errors::QueryString) as Box<Error>);
            }
        }

        Ok(())
    }

    fn parse_utf8(req: &mut backend::Request) -> backend::HandleResult<String> {
        match req.body_mut().read_to_end() {
            Ok(bytes) => {
                 match String::from_utf8(bytes) {
                    Ok(e) => Ok(e),
                    Err(_) => Err(Box::new(
                        errors::Body::new("Invalid UTF-8 sequence".to_string())
                    ) as Box<Error>),
                }
            },
            Err(_) => Err(Box::new(
                errors::Body::new("Invalid request body".to_string())
            ) as Box<Error>),
        }
       
    }

    fn parse_json_body(req: &mut backend::Request, params: &mut json::Object) -> backend::HandleSuccessResult {

        let utf8_string_body = try!(Api::parse_utf8(req));

        if utf8_string_body.len() > 0 {
          let maybe_json_body = utf8_string_body.parse::<json::Json>();
            match maybe_json_body {
                Some(json_body) => {
                    for (key, value) in json_body.as_object().unwrap().iter() {
                        if !params.contains_key(key) {
                            params.insert(key.to_string(), value.clone());
                        }
                    }
                },
                None => return Err(Box::new(errors::Body::new(format!("Invalid JSON"))) as Box<Error>)
            }  
        }

        Ok(())
    }

    fn parse_urlencoded_body(req: &mut backend::Request, params: &mut json::Object) -> backend::HandleSuccessResult {
        let utf8_string_body = try!(Api::parse_utf8(req));

        if utf8_string_body.len() > 0 {
            let maybe_json_body = queryst::parse(utf8_string_body.as_slice());
            match maybe_json_body {
                Ok(json_body) => {
                    for (key, value) in json_body.as_object().unwrap().iter() {
                        if !params.contains_key(key) {
                            params.insert(key.to_string(), value.clone());
                        }
                    }
                },
                Err(_) => return Err(Box::new(errors::Body::new(format!("Invalid encoded data"))) as Box<Error>)
            }  
        }

        Ok(())
    }

    fn parse_request(req: &mut backend::Request, params: &mut json::Object) -> backend::HandleSuccessResult {
        // extend params with query-string params if any
        if req.url().query().is_some() {
            try!(Api::parse_query(req.url().query().as_ref().unwrap().as_slice(), params));   
        }

        // extend params with json-encoded body params if any
        if req.is_json_body() {
            try!(Api::parse_json_body(req, params));
        } else if req.is_urlencoded_body() {
            try!(Api::parse_urlencoded_body(req, params));
        }

        Ok(())
    }

    #[allow(unused_variables)]
    pub fn call<'a>(&self, 
        rest_path: &str, 
        req: &'a mut (backend::Request + 'a), 
        app: &app::Application) -> backend::HandleExtendedResult<backend::Response> {
        
        let mut params = collections::BTreeMap::new();
        let parse_result = Api::parse_request(req, &mut params);

        let api_result = parse_result.and_then(|_| {
            self.api_call(rest_path, &mut params, req, &mut framework::CallInfo::new(app))
        });
        
        match api_result {
            Ok(resp) => Ok(resp),
            Err(err) => {
                let resp = self.handle_error(&err, &self.extract_media(req).unwrap_or_else(|| media::Media::default()));
                Err(backend::ErrorResponse { 
                    error: err,
                    response: resp 
                })
            }
        }
    }
    
}

impl_nesting!(Api);

impl framework::ApiHandler for Api {
    fn api_call<'a, 'r>(&'a self, 
        rest_path: &str, 
        params: &mut json::Object, 
        req: &'r mut (backend::Request + 'r), 
        info: &mut framework::CallInfo<'a>) -> backend::HandleResult<backend::Response> {

        // Check prefix
        let mut rest_path = match self.prefix.as_ref() {
            Some(prefix) => {
                if rest_path.starts_with(prefix.as_slice()) {
                    path::normalize(&rest_path[(prefix.len())..])
                } else {
                   return Err(Box::new(errors::NotMatch) as Box<Error>)
                }
            },
            None => rest_path
        };

        let mut media: Option<media::Media> = None;

        // Check version
        if self.version.is_some() {
            let version_struct = self.version.as_ref().unwrap();
            let ref version = version_struct.version;
            let ref versioning = version_struct.versioning;

            match versioning {
                &Versioning::Path => {
                    if rest_path.starts_with(version.as_slice()) {
                        rest_path = path::normalize(&rest_path[(version.len())..])
                    } else {
                       return Err(Box::new(errors::NotMatch) as Box<Error>)
                    }
                },
                &Versioning::Param(ref param_name) => {
                    match params.get(*param_name) {
                        Some(obj) if obj.is_string() && obj.as_string().unwrap() == version.as_slice() => (),
                        _ => return Err(Box::new(errors::NotMatch) as Box<Error>)
                    }
                },
                &Versioning::AcceptHeader(ref vendor) => {
                    let header = req.headers().get::<header::Accept>();
                    match header {
                        Some(&header::Accept(ref quals)) => {
                            let mut matched_media: Option<media::Media> = None;
                            for qual in quals.iter() {
                                match media::Media::from_vendor(&qual.item) {
                                    Some(media) => {
                                        if media.vendor.as_slice() == *vendor && 
                                           media.version.is_some() && 
                                           media.version.as_ref().unwrap() == version {
                                            matched_media = Some(media);
                                            break;
                                        }
                                    }, 
                                    None => ()
                                }
                            }

                            if matched_media.is_none() {
                                return Err(Box::new(errors::NotMatch) as Box<Error>)
                            } else {
                                media = matched_media;
                            }
                        },
                        None => return Err(Box::new(errors::NotMatch) as Box<Error>)
                    }
                }
            }
        }

        // Check accept media type
        if media.is_none() {
            match self.extract_media(req) {
                Some(media) => {
                    info.media = media
                },
                None => return Err(Box::new(errors::NotAcceptable) as Box<Error>)
            }
        }

        self.push_node(info);
        self.call_handlers(rest_path, params, req, info)
    }
}
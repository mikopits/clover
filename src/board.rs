use std::io::Read;
use std::sync::{Arc, Mutex};

use regex::RegexBuilder;
use chrono::{DateTime, UTC};
use reqwest::StatusCode;

/// A `Board` represents a 4chan board. Automatically caches threads when
/// `catalog` is run. Using `find_cached` or `get_thread` will lazily update
/// the requested thread(s).
#[derive(Debug)]
pub struct Board {
    pub name: String,
    pub client: Arc<Mutex<::Client>>,
    pub thread_cache: Arc<Mutex<::ThreadCache>>,
    catalog_last_modified: Arc<Mutex<Option<DateTime<UTC>>>>
}

impl Board {
    /// Creates a new `Board`.
    pub fn new(client: Arc<Mutex<::Client>>, name: &str) -> ::Result<Board> {
        if !client.lock().unwrap().is_valid_board(name) {
            return Err(::Error::InvalidBoardName)
        }

        Ok(Board {
            client: client,
            name: name.to_string(),
            thread_cache: Arc::new(Mutex::new(::ThreadCache::new())),
            catalog_last_modified: Arc::new(Mutex::new(None))
        })
    }

    /// Get a board's current `Catalog`. Automatically updates the current
    /// thread cache. Returns `Some<Catalog>` if the catalog was updated,
    /// and `None` if the catalog was not modified since the last request.
    pub fn catalog(&self) -> ::Result<Option<Catalog>> {
        let mut res = match *self.catalog_last_modified.lock().unwrap() {
            None => {
                try!(self.client.lock().unwrap().get(
                        &format!("https://a.4cdn.org/{}/catalog.json",
                                 self.name),
                        None))
            },
            Some(dt) => {
                // If-Modified-Since: Sat, 29 Oct 1994 19:43:31 GMT
                //                    %a,  %d %b  %Y   %T       GMT
                let format = "%a, %d %b %Y %T GMT";
                let fmt_date = dt.format(&format).to_string();
                try!(self.client.lock().unwrap().get(
                        &format!("https://a.4cdn.org/{}/catalog.json",
                                 self.name),
                        Some(::IfModifiedSince(fmt_date))))
            }
        };

        match *res.status() {
            StatusCode::Ok => {
                *self.catalog_last_modified.lock().unwrap() = Some(UTC::now());
                let mut buf = String::new();
                try!(res.read_to_string(&mut buf));
                let corrected = r#"{"pages":"#.to_string() + &buf + "}";
                let catalog: Catalog = try!(::serde_json::from_str(&corrected));

                for topic in catalog.topics() {
                    self.thread_cache.lock().unwrap()
                        .insert(::Thread::from_topic(topic.clone(),
                        &self.name, self.client.clone()));
                }

                Ok(Some(catalog))
            },
            StatusCode::NotModified => {
                Ok(None)
            },
            _ => Err(::Error::UnexpectedResponse)
        }
    }

    /// Finds any threads in the cache that contain the query string in one of
    /// the OP's name, comment, subject, or filename. The search is case
    /// insensitive and uses unicode.
    ///
    /// The threads are updated before they are returned. Automatically
    /// excludes expired threads.
    pub fn find_cached(&self, query: &str) -> ::Result<Vec<::Thread>> {
        let mut regex_builder = RegexBuilder::new(query);
        let regex = try!(regex_builder
                         .case_insensitive(true)
                         .unicode(true)
                         .build());

        let mut threads = self.thread_cache.lock().unwrap().threads
            .values()
            .filter(|&t| t.is_match(&regex))
            .cloned()
            .collect::<Vec<::Thread>>();

        // TODO: A returned thread is cloned twice. Needs refactoring.
        let mut return_threads = Vec::new();
        for mut thread in &mut threads {
            try!(thread.update());
            if !thread.expired {
                return_threads.push(thread.clone());
            } else {
                // Update cache, removing expired threads
                self.thread_cache.lock().unwrap().remove(thread.topic.no);
            }
        }

        Ok(threads)
    }

    /// Get a `Thread` that you know the thread number of. First checks that
    /// the thread is in the cache, and updates it if it is. If not, then
    /// makes a request, adds the created struct to the cache, and returns
    /// the thread.
    pub fn get_thread(& self, thread_no: u64) -> ::Result<::Thread> {
        if self.thread_cache.lock().unwrap().contains(thread_no) {
            try!(self.thread_cache.lock().unwrap().threads
                .get_mut(&thread_no)
                .unwrap()
                .update());
            return Ok(self.thread_cache.lock().unwrap()
                      .get(thread_no).unwrap().clone())
        }

        let mut res = try!(self.client.lock().unwrap().get(
                &format!("https://a.4cdn.org/{}/thread/{}.json",
                         self.name, thread_no), None));
        let mut buf = String::new();
        try!(res.read_to_string(&mut buf));
        let deserializer: ::ThreadDeserializer = try!(
            ::serde_json::from_str(&buf));
        let thread = ::Thread::from_deserializer(
            deserializer, &self.name, self.client.clone());
        self.thread_cache.lock().unwrap().insert(thread.clone());

        Ok(thread)
    }
}

/// A `Catalog` contains the information from the 4chan catalog API. Rather
/// than creating `Thread` structs, it contains `Post` structs which represent
/// the thread's topic (aka. OP). If you wish to access the implementation of
/// a `Thread` then use `Board::get_thread` or `Board::find_cached`.
#[derive(Clone, Debug, Deserialize)]
pub struct Catalog {
    pub pages: Vec<Page>
}

impl Catalog {
    pub fn topics(&self) -> Vec<&::Post> {
        self.pages.iter()
            .fold(Vec::new(), |mut topics, p| {
                topics.extend(&p.topics);
                topics
            })
    }

    pub fn find(&self, query: &str) -> ::Result<Option<Vec<&::Post>>> {
        let mut regex_builder = RegexBuilder::new(query);
        let regex = try!(regex_builder
                         .case_insensitive(true)
                         .unicode(true)
                         .build());

        let topics: Vec<&::Post> = self.topics()
            .into_iter()
            .filter(|&t| t.is_match(&regex))
            .collect();

        if topics.is_empty() {
            return Ok(None)
        }

        Ok(Some(topics))
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Page {
    page: u8,
    // Rather than `Thread` objects, pages create a `Post` representing the
    // thread's topic (aka. OP).
    #[serde(rename="threads")]
    pub topics: Vec<::Post>
}

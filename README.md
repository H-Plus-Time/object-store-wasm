## object-store-wasm

A wasm-first implementation of the ObjectStore trait, accounting for the fundamentally non-Send nature of wasm_bindgen_futures::JsFuture (and therefore tacitly assuming single-threaded usage).

Why not just contribute this back to the object_store crate at [apache/arrow-rs](https://github.com/apache/arrow-rs)?

I'd like to, *but* that would require optionalizing most of the Send constraints on the traits in client and http (discounting the several thousand lines of code in aws, gcp, azure), behind a default on feature flag (threadsafe, multithread, whatever). 

I expect there's a few parts of this that could be upstreamed, but I seriously doubt **all** of this belongs upstream. If that turns out to be incorrect, consider this repo an exercise in exterior refactoring.

Roadmap
- [x] get_opts via http/s
- [ ] attempt actual streaming responses
- [ ] avoid/provide a config option for swapping HEAD requests with zero-range GETs
- [ ] JS bindings (behind a flag)
- [ ] AWS read only operations
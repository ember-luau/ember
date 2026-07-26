/*!
Where packages come from: index lookup, dependency resolution, and the
archive publish uploads.

`index` and `resolver` talk to git indices: wally format, pesde format, and
lpm's own (pesde format whose entries bake in a `download` URL, written only
by the registry API). `pack` builds the tarball `lpm publish` sends to that
API (see `net::registry`).
*/

pub mod index;
pub mod pack;
pub mod resolver;

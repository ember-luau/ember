/*!
Where packages come from. Index lookup, dependency resolution, and the
archive publish uploads.

`index` and `resolver` talk to git indices in wally or pesde format, plus
lpm's own, pesde format whose entries bake in a `download` URL and are
written only by the registry API. `pack` builds the tarball `lpm publish`
sends to that API, see `net::registry`.
*/

pub mod index;
pub mod pack;
pub mod resolver;

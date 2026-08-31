// Generated word lists for conductor session names.
//
// Staged alongside the entry module and imported by relative path, which
// works because componentize-qjs resolves imports from the module root with
// a real node resolver. Keep it in source_artifact_names in install.solx.

// Session names are read aloud, typed into every conductor-step call, and
// pasted into scripts, so they are words rather than a timestamp. Lowercase
// letters only: each word is a legal path segment on its own, and no word
// contains a hyphen, so <adjective>-<noun> never needs escaping.
// 130 x 124 = 16120 combinations, which is not enough on its
// own -- see newSessionId, where the collision check lives.
const ADJECTIVES = [
  "amber", "ancient", "autumn", "azure", "blithe", "bold", "boreal", "brave",
  "brisk", "bronze", "calm", "candid", "cheerful", "civic", "clever", "cobalt",
  "copper", "cosmic", "crimson", "crisp", "curious", "dapper", "daring",
  "dauntless", "dawn", "deft", "distant", "dusky", "eager", "early", "earnest",
  "eastern", "easy", "fabled", "fair", "fearless", "fervent", "fleet", "fluent",
  "forest", "frosty", "gallant", "gentle", "gilded", "glad", "gleaming",
  "golden", "graceful", "grand", "hardy", "hazel", "hidden", "humble", "indigo",
  "ivory", "jade", "jolly", "keen", "kindly", "lively", "lucent", "lunar",
  "marble", "mellow", "merry", "mighty", "misty", "modest", "mossy", "nimble",
  "noble", "northern", "olive", "opal", "patient", "placid", "plucky", "polar",
  "prairie", "prompt", "proud", "quiet", "radiant", "rapid", "restless",
  "rugged", "russet", "sable", "saffron", "sage", "sapphire", "scarlet",
  "serene", "silent", "silver", "sleek", "solar", "spirited", "spring", "steady",
  "sterling", "stoic", "stormy", "sunlit", "supple", "swift", "tawny", "tidal",
  "tranquil", "trusty", "twilight", "umber", "upland", "valiant", "velvet",
  "verdant", "vermilion", "vivid", "wandering", "warm", "watchful", "western",
  "whimsical", "willing", "windward", "winter", "wise", "wistful", "woven",
  "zealous"
];

const NOUNS = [
  "albatross", "alder", "anchor", "antler", "arbor", "aspen", "badger", "basalt",
  "beacon", "beetle", "bison", "bramble", "bridge", "brook", "buffalo", "canyon",
  "cardinal", "cedar", "cinder", "clover", "comet", "compass", "condor",
  "coral", "cove", "crane", "crest", "crocus", "current", "cypress", "delta",
  "dolphin", "dune", "eagle", "egret", "elm", "ember", "estuary", "falcon",
  "fathom", "fennec", "fern", "finch", "fjord", "forge", "fossil", "fox",
  "garnet", "geyser", "glacier", "gopher", "granite", "grotto", "harbor",
  "harrier", "hawthorn", "heron", "hollow", "ibis", "inlet", "iris", "jackal",
  "juniper", "kestrel", "kingfisher", "lantern", "larch", "lark", "ledge",
  "lichen", "lynx", "magpie", "mallard", "maple", "marlin", "meadow", "merlin",
  "mesa", "minnow", "moraine", "narwhal", "nettle", "oriole", "osprey", "otter",
  "owl", "panther", "pelican", "pike", "pillar", "pine", "plover", "puffin",
  "quail", "quarry", "quill", "raven", "reef", "ridge", "rowan", "salmon",
  "sandpiper", "sequoia", "shale", "shore", "sparrow", "spruce", "starling",
  "summit", "swallow", "tamarack", "teal", "tern", "thicket", "thistle",
  "thrush", "trout", "tundra", "vulture", "walrus", "warbler", "willow",
  "wolverine", "wren"
];

export { ADJECTIVES, NOUNS };

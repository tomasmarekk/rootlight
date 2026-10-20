//! Verifies the reviewed Tree-sitter source, license, and Cargo dependency lock.
//!
//! The manifest is intentionally redundant with Cargo metadata: policy checks
//! fail if a dependency update changes either executable code or audit evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use crate::{
    cargo_metadata::{Dependency, DependencyKind, Metadata},
    native_table_bounds,
};
use serde::Deserialize;

const GRAMMAR_LOCK_PATH: &str = "adapters/grammars.lock";
const CARGO_LOCK_PATH: &str = "Cargo.lock";
const ADAPTER_PACKAGE: &str = "rootlight-adapter-treesitter";
const GRAMMAR_LOCK_SHA256: &str =
    "ad5de3ad5f23feec32379f909417b06f57ace16a5cbbbe824641266ff3ea6efc";
const JAVA_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-java-0.23.5-LICENSE";
const JAVA_LICENSE_SHA256: &str =
    "52ed137b039cd9c46409bc22e89938af911c95b157feae2d040b51e6084369a7";
const TYPESCRIPT_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-typescript-0.23.2-LICENSE";
const TYPESCRIPT_LICENSE_SHA256: &str =
    "49bf33cf78ef5897e4e161ce1517df7de1ae5042a65b6bcfd44401e0fc606559";
const CPP_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-cpp-0.23.4-LICENSE";
const CPP_LICENSE_SHA256: &str = "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1";
const KOTLIN_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-kotlin-ng-1.1.0-LICENSE";
const KOTLIN_LICENSE_SHA256: &str =
    "0eea8dc45e89deeb03c7799bbbc7b4688f365fb274562f4540ecfebdea82e727";
const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const LUA_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-lua-0.5.0-LICENSE";
const LUA_LICENSE_SHA256: &str = "9a32b02e4c917b1ce6b5e79d8ea81e25cefd7f27d89c7235f2afb262c06cf32e";

const RUBY_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-ruby-0.23.1-LICENSE";
const RUBY_LICENSE_SHA256: &str =
    "ee006f02a3d856df282e409be2a86e24a65bb573a98b9c28343771141351bb6b";
const COQ_LICENSE_DECLARATIONS_PATH: &str =
    "third_party/tree-sitter-rocq-0.2.0/upstream/license-declarations.json";
const COQ_LICENSE_DECLARATIONS_SHA256: &str =
    "c820cf5ea9a442518dd554464086e9b733948518c9de4b6b9deb2168fb8ab57e";

const EXPECTED_PACKAGES: [(&str, &str, &str); 35] = [
    (
        "tree-sitter",
        "0.26.11",
        "af1c71c1c4cc0920b20d6b0f6572e7682cd07a6a2faec71067a31fa394c586df",
    ),
    (
        "dekobon-tree-sitter-groovy",
        "0.2.0",
        "891a2367f48836c854e84bac87f17513707a65a20dce7cab5707ddde6ca23fe0",
    ),
    (
        "ts-parser-perl",
        "2.0.0",
        "db3cd8574afc19af4d3db44fe0cf94a5e8fc3f4056c0326baf5ba01a29666129",
    ),
    (
        "tree-sitter-matlab",
        "1.3.0",
        "8e8d8831a78547f54860dd8467166b1ab0c466d03d43c4bb447af55c024281f7",
    ),
    (
        "tree-sitter-nix",
        "0.3.0",
        "4952a9733f3a98f6683a0ccd1035d84ab7a52f7e84eeed58548d86765ad92de3",
    ),
    (
        "tree-sitter-objc",
        "3.0.2",
        "9ca8bb556423fc176f0535e79d525f783a6684d3c9da81bf9d905303c129e1d2",
    ),
    (
        "tree-sitter-astro-next",
        "0.1.1",
        "794a4a59fc2d88e49b4bc41fef9522d77184a36f4e68bbaf545cd1eb2364c46e",
    ),
    (
        "tree-sitter-md",
        "0.5.3",
        "2efd398be546456c814598ee56c0f51769a77241511b4a58077815d120afa882",
    ),
    (
        "tree-sitter-powershell",
        "0.26.4",
        "3faf304d44b9ddd4a7d97804bb8de7daf564336dd5a526dc6de5b39238243022",
    ),
    (
        "tree-sitter-dart",
        "0.2.0",
        "325dd1e24ee9ee21111e9c43680ae7d6010aaa9f282b048a99b9c7163c1cf553",
    ),
    (
        "tree-sitter-scala",
        "0.26.2",
        "24e0ab4505990bfe30051761d40a7bf4033ce5a81c9eda9e20e987a5cdc84826",
    ),
    (
        "tree-sitter-r",
        "1.3.0",
        "ffc9954ec870dcad6cffdd302b405306c68cf031ed79a78cd9746f6740d9fe20",
    ),
    (
        "tree-sitter-sequel",
        "0.3.11",
        "9d198ad3c319c02e43c21efa1ec796b837afcb96ffaef1a40c1978fbdcec7d17",
    ),
    (
        "tree-sitter-bash",
        "0.25.1",
        "9e5ec769279cc91b561d3df0d8a5deb26b0ad40d183127f409494d6d8fc53062",
    ),
    (
        "tree-sitter-c",
        "0.24.2",
        "a9b2eb57a55fed6b00812912e730b7a275cf4fe98bfd6a5d76263d4438371728",
    ),
    (
        "tree-sitter-c-sharp",
        "0.23.5",
        "c1aac67f1ad71de1d6d39708d34811081c26dfa495658de6c14c34200849357c",
    ),
    (
        "tree-sitter-cpp",
        "0.23.4",
        "df2196ea9d47b4ab4a31b9297eaa5a5d19a0b121dceb9f118f6790ad0ab94743",
    ),
    (
        "tree-sitter-go",
        "0.25.0",
        "c8560a4d2f835cc0d4d2c2e03cbd0dde2f6114b43bc491164238d333e28b16ea",
    ),
    (
        "tree-sitter-java",
        "0.23.5",
        "0aa6cbcdc8c679b214e616fd3300da67da0e492e066df01bcf5a5921a71e90d6",
    ),
    (
        "tree-sitter-json",
        "0.24.8",
        "4d727acca406c0020cffc6cf35516764f36c8e3dc4408e5ebe2cb35a947ec471",
    ),
    (
        "tree-sitter-kotlin-ng",
        "1.1.0",
        "e800ebbda938acfbf224f4d2c34947a31994b1295ee6e819b65226c7b51b4450",
    ),
    (
        "tree-sitter-php",
        "0.24.2",
        "0d8c17c3ab69052c5eeaa7ff5cd972dd1bc25d1b97ee779fec391ad3b5df5592",
    ),
    (
        "tree-sitter-lua",
        "0.5.0",
        "8daaf5f4235188a58603c39760d5fa5d4b920d36a299c934adddae757f32a10c",
    ),
    (
        "tree-sitter-python",
        "0.25.0",
        "6bf85fd39652e740bf60f46f4cda9492c3a9ad75880575bf14960f775cb74a1c",
    ),
    (
        "tree-sitter-rust",
        "0.24.2",
        "439e577dbe07423ec2582ac62c7531120dbfccfa6e5f92406f93dd271a120e45",
    ),
    (
        "tree-sitter-ruby",
        "0.23.1",
        "be0484ea4ef6bb9c575b4fdabde7e31340a8d2dbc7d52b321ac83da703249f95",
    ),
    (
        "tree-sitter-typescript",
        "0.23.2",
        "6c5f76ed8d947a75cc446d5fccd8b602ebf0cde64ccf2ffa434d873d7a575eff",
    ),
    (
        "tree-sitter-toml-ng",
        "0.7.0",
        "e9adc2c898ae49730e857d75be403da3f92bb81d8e37a2f918a08dd10de5ebb1",
    ),
    (
        "tree-sitter-swift",
        "0.7.3",
        "fe36052155b9dd69ca82b3b8f1b4ccfb2d867125ac1a4db1dd7331829242668c",
    ),
    (
        "tree-sitter-css",
        "0.25.0",
        "a5cbc5e18f29a2c6d6435891f42569525cf95435a3e01c2f1947abcde178686f",
    ),
    (
        "tree-sitter-yaml",
        "0.7.2",
        "53c223db85f05e34794f065454843b0668ebc15d240ada63e2b5939f43ce7c97",
    ),
    (
        "tree-sitter-html",
        "0.23.2",
        "261b708e5d92061ede329babaaa427b819329a9d427a1d710abb0f67bbef63ee",
    ),
    (
        "tree-sitter-solidity",
        "1.2.13",
        "4eacf8875b70879f0cb670c60b233ad0b68752d9e1474e6c3ef168eea8a90b25",
    ),
    (
        "tree-sitter-haskell",
        "0.23.1",
        "977c51e504548cba13fc27cb5a2edab2124cf6716a1934915d07ab99523b05a4",
    ),
    (
        "tree-sitter-fortran",
        "0.6.0",
        "ea5fea41363a9b59666001dc26c4b356d4e164da27c6ce31145e00aba6bf9b16",
    ),
];

const EXPECTED_LOCAL_PACKAGES: [(&str, &str, &str, &str); 47] = [
    (
        "tree-sitter-qsharp",
        "0.2.0",
        "third_party/tree-sitter-qsharp-0.2.0",
        "e3d9b65ff4c70ff8ac3e624d6be881187867b7715e87318932894e43fe67f045",
    ),
    (
        "tree-sitter-sml",
        "0.25.0",
        "third_party/tree-sitter-sml-0.25.0",
        "f1fca01af0d0766f2553929872f0512590d05ebd10d1bfe4df519d58b89142d9",
    ),
    (
        "tree-sitter-wat",
        "0.1.0",
        "third_party/tree-sitter-wat-0.1.0",
        "3a95933fe9bafb0214c990ed1b64aa3407fa9dd7ee2a98831f96e1da4d77f166",
    ),
    (
        "tree-sitter-mojo",
        "0.25.0",
        "third_party/tree-sitter-mojo-0.25.0",
        "4b55cfd6df9be7f5c2213c6e065bd311b17d5de12f266937af9f5b4fb25fe5fe",
    ),
    (
        "tree-sitter-hare",
        "1.0.0",
        "third_party/tree-sitter-hare-1.0.0",
        "effdf64e7d6f3b777ff3eb4be2abe8a891cea0b982904dd6dee852aea387fdaa",
    ),
    (
        "tree-sitter-ring",
        "1.1.1",
        "third_party/tree-sitter-ring-1.1.1",
        "ea35f0aea7883c2b80dc0c2e047004942daa11dbd7c1277c3bc2a46bef125069",
    ),
    (
        "tree-sitter-haxe",
        "0.13.0",
        "third_party/tree-sitter-haxe-0.13.0",
        "d294a758649b01dab6007beacdf0788310461427c0fc0cb580cc491776237bc1",
    ),
    (
        "tree-sitter-qmljs",
        "0.3.1",
        "third_party/tree-sitter-qmljs-0.3.1",
        "1a8c3a2c14859d4a4ff4ba18a461dabd63741d65eab3945209c22bff67a658e4",
    ),
    (
        "tree-sitter-modelica",
        "0.0.1",
        "third_party/tree-sitter-modelica-0.0.1",
        "69e459f26663225b7c72108fbd30ce4eace8ad9c17f1184ad4b295dcd7ae6700",
    ),
    (
        "tree-sitter-d",
        "0.8.2",
        "third_party/tree-sitter-d-0.8.2",
        "e14c52bac0819be1671209289cee8128736cfe26a969a6d55362bc7c98b5c4de",
    ),
    (
        "tree-sitter-ada",
        "0.9.1",
        "third_party/tree-sitter-ada-0.9.1",
        "16926552494876001b7cfca6f7147d3d253d4f328840ee9a7ac512fdea137ae9",
    ),
    (
        "tree-sitter-hcl",
        "1.2.0",
        "third_party/tree-sitter-hcl-1.2.0",
        "a990425a19545a653a8935bc36c53bc6027319a379c06538ec42533c2e286b09",
    ),
    (
        "tree-sitter-zig",
        "1.1.2",
        "third_party/tree-sitter-zig-1.1.2",
        "e5bc165112d0efca8a1882e84a7b222f3bc9efdc34be2252027fc9c51bdd7a55",
    ),
    (
        "tree-sitter-crystal",
        "0.0.1",
        "third_party/tree-sitter-crystal-0.0.1",
        "2dbe11c4d049948d2b667cd45b96590c9b8902e029552ee0c2c8cfbb5c43dbbf",
    ),
    (
        "tree-sitter-cue",
        "0.2.0",
        "third_party/tree-sitter-cue-0.2.0",
        "957d595e88861fdf80a0d880b50eb7669712012babc398a6243217cb05b542d4",
    ),
    (
        "tree-sitter-pony",
        "0.0.3",
        "third_party/tree-sitter-pony-0.0.3",
        "3476689cb3331c29f9adaa65b7514679969b0358564bcaaf9a5edfde6822ccd0",
    ),
    (
        "tree-sitter-fish",
        "3.7.0",
        "third_party/tree-sitter-fish-3.7.0",
        "0c2760fdf948fd9ad7e639f30c63b4883ba62742af920f4cf35fa8950e575722",
    ),
    (
        "tree-sitter-nim",
        "0.6.2",
        "third_party/tree-sitter-nim-0.6.2",
        "5008548d97680bc3738f4a50dafabedcae6f8f82c3f65c59daf506eb91d2ebc9",
    ),
    (
        "tree-sitter-clojure",
        "0.0.8",
        "third_party/tree-sitter-clojure-0.0.8",
        "9c5f3779b1502abec604979b6115a73fb585558f64d47e61f9fa3be6c0ababa6",
    ),
    (
        "tree-sitter-racket",
        "0.25.0",
        "third_party/tree-sitter-racket-0.25.0",
        "c3b1ef220aebf912fc0e19a230e062043d2f129fa7f2865b66306dc5f730b815",
    ),
    (
        "tree-sitter-vhdl",
        "2.0.3",
        "third_party/tree-sitter-vhdl-2.0.3",
        "5eaa9e956b862a7f158d326841de4eeb9e35d3cba262bfeab8f220e77613f854",
    ),
    (
        "tree-sitter-graphql",
        "0.0.1",
        "third_party/tree-sitter-graphql-0.0.1",
        "cfc60a9394a5c0eca78227ba81633573804ccf91a85dd94de154ecab56b11aeb",
    ),
    (
        "tree-sitter-elixir",
        "0.3.5",
        "third_party/tree-sitter-elixir-0.3.5",
        "fb7e09181eb45c78d55c96d15f1635af7c66a0e4cee4af3872152674f7d1cfaa",
    ),
    (
        "tree-sitter-proto",
        "0.0.1",
        "third_party/tree-sitter-proto-0.0.1",
        "c769d77682fecd620789db08607db7e0aee26125645bda92f6f1515927d2423e",
    ),
    (
        "tree-sitter-thrift",
        "0.5.0",
        "third_party/tree-sitter-thrift-0.5.0",
        "f4cec11118d267928a0471b9b3faad3f24d18201e76d326ed50fc6f878731b82",
    ),
    (
        "tree-sitter-capnp",
        "1.5.0",
        "third_party/tree-sitter-capnp-1.5.0",
        "5eadccfa26383aca16f8ef7348f8bb174efb1ff51f528432e3e89a22ec96eab4",
    ),
    (
        "tree-sitter-rocq",
        "0.2.0",
        "third_party/tree-sitter-rocq-0.2.0",
        "784a5319e136d5917ac3f171f3f48f15e88785cf72f961707c9bbe2888b975de",
    ),
    (
        "tree-sitter-v",
        "0.0.6",
        "third_party/tree-sitter-v-0.0.6",
        "41b362b3f2796828ad87b08f44083ec10c280f0bb4cbf838d83d447591ef5624",
    ),
    (
        "tree-sitter-systemverilog",
        "0.4.1",
        "third_party/tree-sitter-systemverilog-0.4.1",
        "85929d95aed3c1f0b6137d80e22d6fbd852d464bc09e7915480320ffa139855a",
    ),
    (
        "tree-sitter-eiffel",
        "1.0.0",
        "third_party/tree-sitter-eiffel-1.0.0",
        "ba0e42b1159dcca70d425be406085b7db30c17746d3fe5d8ff668ea3da54a24a",
    ),
    (
        "tree-sitter-julia",
        "0.25.0",
        "third_party/tree-sitter-julia-0.25.0",
        "4a66664bee7741c0b7d75a3e384338888895e0c2c911a6b05afb6faeec563f6f",
    ),
    (
        "tree-sitter-cmake",
        "0.7.5",
        "third_party/tree-sitter-cmake-0.7.5",
        "49cb0fc86bafe35be60abd17eece4a21bc9a4abb4ab74dbd105fb413b4e65a4c",
    ),
    (
        "tree-sitter-sas",
        "0.4.2",
        "third_party/tree-sitter-sas-0.4.2",
        "8d9e9235ecabaae7f012b1d1b654171cefc4d2ede9b5b994bcbaa582a4405e13",
    ),
    (
        "tree-sitter-wgsl",
        "0.0.9",
        "third_party/tree-sitter-wgsl-0.0.9",
        "4a6bd6793fc477b90b2ded2d5c7f449485e1c19473fad36b90fb8777e0b60b3e",
    ),
    (
        "tree-sitter-xml",
        "0.7.0",
        "third_party/tree-sitter-xml-0.7.0",
        "4e87a41d7b5cb7f3d7f8931db78c4f0050f221257ac65fc0a7a213829ba97ef0",
    ),
    (
        "tree-sitter-asm",
        "0.24.0",
        "third_party/tree-sitter-asm-0.24.0",
        "05ab716e887d01045e6357cbce255e427546bc540035703105de8a42d2a1a95e",
    ),
    (
        "tree-sitter-glsl",
        "0.2.0",
        "third_party/tree-sitter-glsl-0.2.0",
        "64c10d88da00b674b3e7280b4bfa12650a49f88a1e4c00a55e76798823c83815",
    ),
    (
        "tree-sitter-hlsl",
        "0.2.0",
        "third_party/tree-sitter-hlsl-0.2.0",
        "bc876400ce323fce73c6927f3fff9c92be29fde8ebb90fb60e67542f53c8c516",
    ),
    (
        "tree-sitter-commonlisp",
        "0.4.1",
        "third_party/tree-sitter-commonlisp-0.4.1",
        "f07da8ce6df2522d0ae98708338b2cb4b50c0be8ac9e052976ad5736c70bf462",
    ),
    (
        "tree-sitter-scheme-native-local",
        "0.24.7-1",
        "third_party/tree-sitter-scheme-0.24.7-1",
        "d1477c4b209d9db6337e4a646cf8c2134422b432f14c00e74068ef2105829b6f",
    ),
    (
        "tree-sitter-ocaml",
        "0.25.0",
        "third_party/tree-sitter-ocaml-0.25.0",
        "544e58ed05a66a74ef293db0c2825003d94c6e8426c38aff5603e3743c183f10",
    ),
    (
        "tree-sitter-pascal",
        "0.10.2",
        "third_party/tree-sitter-pascal-0.10.2",
        "4ce8bf2245c875e3009bb978bc9888c558c5c17c740bdae89e78c3837e0be98a",
    ),
    (
        "tree-sitter-sfapex",
        "3.0.1",
        "third_party/tree-sitter-sfapex-3.0.1",
        "13538adab5f9df36571a3f4541b5bc8081b1fc857735db0f1aef397d17b435ed",
    ),
    (
        "tree-sitter-vba",
        "0.14.1",
        "third_party/tree-sitter-vba-0.14.1",
        "058debd1085c94a23989976f4c9289e30bbde00ee5b4e729f8185362ba972560",
    ),
    (
        "tree-sitter-cobol",
        "0.0.1",
        "third_party/tree-sitter-cobol-0.0.1",
        "de05274f23c60ac2cb9a90af56d9eedb6f7aa94e77a1407b87c178202e6eafb8",
    ),
    (
        "tree-sitter-plsql",
        "0.0.1",
        "third_party/tree-sitter-plsql-0.0.1",
        "f3ca99f350039320e3d687eab76b8039195123bc9c3a03f7ccabd221a817e031",
    ),
    (
        "tree-sitter-fsharp",
        "0.3.12",
        "third_party/tree-sitter-fsharp-0.3.12",
        "813a911d6caba550be7af1b369c15df347f80a07eab8f3720ea9da476693b37a",
    ),
];

pub(crate) fn check(metadata: &Metadata, root: &Path) -> Result<(), GrammarLockError> {
    let path = root.join(GRAMMAR_LOCK_PATH);
    let bytes = fs::read(&path).map_err(|source| GrammarLockError::Read {
        path: path.clone(),
        source,
    })?;
    require_digest(GRAMMAR_LOCK_PATH, &bytes, GRAMMAR_LOCK_SHA256)?;
    let text = std::str::from_utf8(&bytes).map_err(|source| GrammarLockError::Utf8 {
        path: path.clone(),
        source,
    })?;
    let manifest: GrammarLock = toml::from_str(text).map_err(|source| GrammarLockError::Parse {
        path: path.clone(),
        source,
    })?;
    validate_manifest(&manifest)?;
    validate_direct_dependencies(metadata, root)?;
    validate_cargo_lock(root, &manifest)?;
    validate_vendored_sources(metadata, root, &manifest)?;

    validate_local_license(
        root,
        COQ_LICENSE_DECLARATIONS_PATH,
        COQ_LICENSE_DECLARATIONS_SHA256,
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-v-0.0.6-LICENSE",
        "abfabe98680cd5bef27c8ca64549b2f45df38f028509fe6f9eca7a52a5af72f6",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-systemverilog-0.4.1-LICENSE",
        "f4ceaa91e73e623d6b30895a3b644e0e277754a81c8b28655b75d7a9113f1ac4",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-eiffel-1.0.0-LICENSE",
        "8a138713a72dc117037c40c7b5a587649a7e96aa0dc4c4517fc947173348c0c7",
    )?;
    validate_local_license(root, JAVA_LICENSE_PATH, JAVA_LICENSE_SHA256)?;
    validate_local_license(root, TYPESCRIPT_LICENSE_PATH, TYPESCRIPT_LICENSE_SHA256)?;
    validate_local_license(root, CPP_LICENSE_PATH, CPP_LICENSE_SHA256)?;
    validate_local_license(root, KOTLIN_LICENSE_PATH, KOTLIN_LICENSE_SHA256)?;
    validate_local_license(root, LUA_LICENSE_PATH, LUA_LICENSE_SHA256)?;
    validate_local_license(root, RUBY_LICENSE_PATH, RUBY_LICENSE_SHA256)?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-julia-0.25.0-LICENSE",
        "74f800107f200f4e28da33e9c3f86230d37d8f4c0a4916f7111e800c395ee3a2",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-cmake-0.7.5-LICENSE",
        "97334c0f9c0962b33cbe03223f584b8ed64c2d14419ce80cc2e945eb8d367d28",
    )?;
    validate_local_license(
        root,
        "third_party/tree-sitter-asm-0.24.0/notices/LICENSE",
        "0e9f43573925c2e4b49f328a45c4df22c882191d77d876b0d18ecd60bd21e17c",
    )?;
    validate_local_license(
        root,
        "third_party/tree-sitter-sfapex-3.0.1/LICENSE",
        "c3d045abbc0a8dec1b00737c0668f6dbb028d248521b74e79778c726671925ed",
    )?;
    validate_local_license(
        root,
        "third_party/tree-sitter-sfapex-3.0.1/LICENSE-JAVA",
        JAVA_LICENSE_SHA256,
    )?;
    validate_local_license(
        root,
        "third_party/tree-sitter-hlsl-0.2.0/LICENSE",
        "dec735ccc031754dd05e6556dd9e959915eb367cf01cf009c77a1a38786932b1",
    )?;
    validate_local_license(
        root,
        "third_party/tree-sitter-hlsl-0.2.0/notices/tree-sitter-cpp-LICENSE",
        "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1",
    )?;
    validate_local_license(
        root,
        "third_party/tree-sitter-hlsl-0.2.0/notices/tree-sitter-c-LICENSE",
        "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1",
    )?;
    validate_local_license(
        root,
        "third_party/tree-sitter-pascal-0.10.2/LICENSE",
        "2e2fcc500eacb15b76c11fd2229aebf0a5b4c8ed476f068447c6dce778a17ac9",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/dekobon-tree-sitter-groovy-0.2.0-LICENSE-MIT",
        "7846ff7803888ebb32f936a8a858eb9d5d73560401d92a2f358d562642205263",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/dekobon-tree-sitter-groovy-0.2.0-LICENSE-APACHE",
        "c95bae1d1ce0235ecccd3560b772ec1efb97f348a79f0fbe0a634f0c2ccefe2c",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/ts-parser-perl-2.0.0-LICENSE",
        "68a9a526ae357ed5a2f8ca5dabb14131b283554b69639bb04816088d4b1f2fa0",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-matlab-1.3.0-LICENSE",
        "8673f5acd59bfb3bd129760a57f72092d5e3c01e05837532eac9497cf8006a46",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-nix-0.3.0-LICENSE",
        "c1b72e50266464ec5118d26200e17bf1f472d142e62ab637e1ffc23986657a78",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-objc-3.0.2-LICENSE",
        "099c44248f8cf353123211318680e93465587c005a4b3730ce8cb5334de043d6",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-objc-3.0.2-LICENSE-C",
        "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-astro-next-0.1.1-LICENSE",
        "62c7a1e35f56406896d7aa7ca52d0cc0d272ac022b5d2796e7d6905db8a3636a",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-astro-next-0.1.1-LICENSE-MIT",
        "9ec95a90150fa19e2cee78faf49138fc480f6f7a2354006a0ab150c44239447e",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-md-0.5.3-LICENSE",
        "52ec8a1bf8256511e2a92613e0bb41ffefba3897f16fa958d7bb28c466dd4804",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-powershell-0.26.4-LICENSE",
        "1fca8454c0d77a73f9922a52d39e1f7c339f5da5d03f2766eff66b859fa19477",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-r-1.3.0-LICENSE",
        "de2e49529f03d573bc3fa229dc83acfe22c63a5ad1766b289563edfd45b72dea",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-sequel-0.3.11-LICENSE",
        "3b3e4e4252d5d7d1c18e1257005f23242bf0580ad619204fd093c99b8c56748d",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-xml-0.7.0-LICENSE",
        "3297073719030b7261afd78b7267931ccc03a27f7a65ca6fc644316180f71bf6",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-0.26.11-LICENSE",
        "c5cfb43042b6b72045f4ba997834d0a7786d2793d91680868b5815b39f14fc78",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-html-0.23.2-LICENSE",
        "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-yaml-0.7.2-LICENSE",
        "2d181298822a124f6d106118dcc69c8d4d26ac668559be29682c452e712b0947",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-toml-ng-0.7.0-LICENSE",
        "b727eee929bee836e8102cecf749a795b199f291b58c885bcd81cae727ac5a46",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-json-0.24.8-LICENSE",
        "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-bash-0.25.1-LICENSE",
        "49bf33cf78ef5897e4e161ce1517df7de1ae5042a65b6bcfd44401e0fc606559",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-css-0.25.0-LICENSE",
        "c5cfb43042b6b72045f4ba997834d0a7786d2793d91680868b5815b39f14fc78",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-swift-0.7.3-LICENSE",
        "3533cec129bb4bba015c0d61d86dd7c3b7e82110e4d2ff7837a01eff5bad5ccc",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-haskell-0.23.1-LICENSE",
        "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1",
    )?;
    Ok(())
}

fn validate_local_license(
    root: &Path,
    relative_path: &'static str,
    expected_sha256: &str,
) -> Result<(), GrammarLockError> {
    let path = root.join(relative_path);
    let bytes = fs::read(&path).map_err(|source| GrammarLockError::Read { path, source })?;
    require_digest(relative_path, &bytes, expected_sha256)
}

fn validate_manifest(manifest: &GrammarLock) -> Result<(), GrammarLockError> {
    if manifest.schema_version != "1.0" {
        return Err(GrammarLockError::UnsupportedVersion(
            manifest.schema_version.clone(),
        ));
    }
    validate_runtime(&manifest.runtime)?;
    if manifest.grammars.len() != 87 {
        return Err(GrammarLockError::GrammarCount(manifest.grammars.len()));
    }
    let mut languages = BTreeSet::new();
    let mut packages = BTreeMap::new();
    for grammar in &manifest.grammars {
        validate_grammar(grammar)?;
        if !languages.insert(grammar.language.as_str()) {
            return Err(GrammarLockError::DuplicateLanguage(
                grammar.language.clone(),
            ));
        }
        let identity = (
            grammar.crate_version.as_str(),
            grammar.crates_io_checksum.as_deref(),
        );
        if let Some(previous) = packages.insert(grammar.crate_name.as_str(), identity)
            && previous != identity
        {
            return Err(GrammarLockError::PackageEvidence {
                package: grammar.crate_name.clone(),
            });
        }
    }
    let expected_languages = BTreeSet::from([
        "qsharp",
        "standard-ml",
        "webassembly",
        "mojo",
        "hare",
        "ring",
        "haxe",
        "qml",
        "modelica",
        "d",
        "ada",
        "hcl",
        "zig",
        "crystal",
        "cue",
        "pony",
        "fish",
        "nim",
        "clojure",
        "racket",
        "vhdl",
        "graphql",
        "elixir",
        "protobuf",
        "thrift",
        "capnp",
        "coq",
        "v",
        "verilog",
        "systemverilog",
        "eiffel",
        "julia",
        "cmake",
        "apex",
        "assembly",
        "astro",
        "bash",
        "c",
        "cobol-fixed",
        "cobol-free",
        "common-lisp",
        "cpp",
        "csharp",
        "css",
        "dart",
        "dtd",
        "fsharp",
        "fsharp-signature",
        "fortran",
        "glsl",
        "go",
        "groovy",
        "haskell",
        "hlsl",
        "html",
        "sql",
        "java",
        "javascript",
        "json",
        "kotlin",
        "lua",
        "markdown",
        "matlab",
        "nix",
        "objective-c",
        "ocaml",
        "ocaml-interface",
        "pascal",
        "perl",
        "plsql",
        "php",
        "powershell",
        "python",
        "r",
        "solidity",
        "scala",
        "scheme",
        "ruby",
        "rust",
        "sas",
        "swift",
        "toml",
        "xml",
        "yaml",
        "typescript",
        "vba",
        "wgsl",
    ]);
    if languages != expected_languages {
        return Err(GrammarLockError::LanguageSet);
    }
    for (name, version, checksum) in EXPECTED_PACKAGES.iter().skip(1) {
        if packages.get(name) != Some(&(*version, Some(*checksum))) {
            return Err(GrammarLockError::PackageEvidence {
                package: (*name).to_owned(),
            });
        }
    }
    for (name, version, directory, checksum) in EXPECTED_LOCAL_PACKAGES {
        let grammars: Vec<_> = manifest
            .grammars
            .iter()
            .filter(|grammar| grammar.crate_name == name)
            .collect();
        if grammars.is_empty() {
            return Err(GrammarLockError::PackageEvidence {
                package: name.to_owned(),
            });
        }
        // Multi-profile bindings must retain one complete source identity for
        // every language record, not only the first matching package entry.
        for grammar in grammars {
            if grammar.crate_version != version
                || grammar.crates_io_checksum.is_some()
                || grammar.vendored_source_sha256.as_deref() != Some(checksum)
                || grammar
                    .vendored
                    .as_ref()
                    .map(|vendored| vendored.manifest_path.as_str())
                    != Some(format!("{directory}/Cargo.toml").as_str())
            {
                return Err(GrammarLockError::PackageEvidence {
                    package: name.to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn validate_runtime(runtime: &RuntimeEvidence) -> Result<(), GrammarLockError> {
    let (expected_name, expected_version, expected_checksum) = EXPECTED_PACKAGES[0];
    if runtime.crate_name != expected_name
        || runtime.crate_version != expected_version
        || runtime.crates_io_checksum != expected_checksum
        || runtime.repository != "https://github.com/tree-sitter/tree-sitter"
        || runtime.tag != "v0.26.11"
        || runtime.commit != "64402de2857cc197ecc4ca3bc144ea91fda7e72e"
        || runtime.abi_minimum != 13
        || runtime.abi_current != 15
        || runtime.license != "MIT"
        || runtime.msrv != "1.77"
    {
        return Err(GrammarLockError::RuntimeEvidence);
    }
    validate_sha256("runtime crate checksum", &runtime.crates_io_checksum)?;
    validate_sha256("runtime license", &runtime.license_sha256)?;
    require_nonempty("runtime license_source", &runtime.license_source)?;
    require_nonempty("runtime offline_behavior", &runtime.offline_behavior)?;
    require_nonempty_collection("runtime capabilities", &runtime.capabilities)?;
    require_nonempty_collection("runtime parser_limits", &runtime.parser_limits)
}

fn validate_grammar(grammar: &GrammarEvidence) -> Result<(), GrammarLockError> {
    for (field, value) in [
        ("language", grammar.language.as_str()),
        ("crate_name", grammar.crate_name.as_str()),
        ("crate_version", grammar.crate_version.as_str()),
        ("repository", grammar.repository.as_str()),
        ("tag", grammar.tag.as_str()),
        ("commit", grammar.commit.as_str()),
        ("license", grammar.license.as_str()),
        ("license_source", grammar.license_source.as_str()),
        ("modifications", grammar.modifications.as_str()),
        ("test_corpus", grammar.test_corpus.as_str()),
        ("msrv", grammar.msrv.as_str()),
        ("offline_behavior", grammar.offline_behavior.as_str()),
        ("analysis_tier", grammar.analysis_tier.as_str()),
        ("semantic_depth", grammar.semantic_depth.as_str()),
    ] {
        require_nonempty(field, value)?;
    }
    validate_source_provenance(grammar)?;
    validate_sha256("generated parser", &grammar.parser_sha256)?;
    if grammar.scanner_sha256 != "none" {
        validate_sha256("generated scanner", &grammar.scanner_sha256)?;
    }
    validate_sha256("grammar license", &grammar.license_sha256)?;
    if !(13..=15).contains(&grammar.abi) {
        return Err(GrammarLockError::GrammarAbi {
            language: grammar.language.clone(),
            abi: grammar.abi,
        });
    }
    if grammar.analysis_tier != "D" {
        return Err(GrammarLockError::InvalidAnalysisTier {
            language: grammar.language.clone(),
        });
    }
    require_nonempty_collection("grammar capabilities", &grammar.capabilities)?;
    if grammar.language == "rust" && grammar.audit_notes.len() != 2 {
        return Err(GrammarLockError::MissingAuditCaveat("rust"));
    }
    if grammar.language == "java"
        && (grammar.audit_notes.len() != 2 || grammar.license_source != JAVA_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat("java"));
    }
    if matches!(grammar.language.as_str(), "javascript" | "typescript")
        && (grammar.audit_notes.len() != 2 || grammar.license_source != TYPESCRIPT_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat(
            if grammar.language == "javascript" {
                "javascript"
            } else {
                "typescript"
            },
        ));
    }
    if grammar.language == "cpp"
        && (grammar.audit_notes.len() != 3 || grammar.license_source != CPP_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat("cpp"));
    }
    if grammar.language == "kotlin"
        && (grammar.audit_notes.len() != 3 || grammar.license_source != KOTLIN_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat("kotlin"));
    }
    if grammar.language == "lua"
        && (grammar.audit_notes.len() != 3 || grammar.license_source != LUA_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat("lua"));
    }
    if grammar.language == "ruby"
        && (grammar.audit_notes.len() != 3 || grammar.license_source != RUBY_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat("ruby"));
    }
    if grammar.language == "coq"
        && (grammar.license != "MIT"
            || grammar.license_source != COQ_LICENSE_DECLARATIONS_PATH
            || grammar.license_sha256 != COQ_LICENSE_DECLARATIONS_SHA256
            || grammar
                .vendored
                .as_ref()
                .and_then(|vendored| vendored.license_path.as_deref())
                != Some("upstream/license-declarations.json")
            || grammar.audit_notes.len() != 4)
    {
        return Err(GrammarLockError::MissingAuditCaveat("coq"));
    }
    Ok(())
}

fn validate_source_provenance(grammar: &GrammarEvidence) -> Result<(), GrammarLockError> {
    match (
        grammar.crates_io_checksum.as_deref(),
        grammar.vendored_source_sha256.as_deref(),
        grammar.vendored.as_ref(),
    ) {
        (Some(checksum), None, _) => validate_sha256("grammar crate checksum", checksum),
        (None, Some(checksum), Some(vendored)) => {
            validate_sha256("vendored source manifest", checksum)?;
            let observed = vendored_source_digest(&vendored.files)?;
            if observed != checksum {
                return Err(GrammarLockError::DigestMismatch {
                    label: "vendored source manifest",
                    expected: checksum.to_owned(),
                    observed,
                });
            }
            Ok(())
        }
        _ => Err(GrammarLockError::SourceProvenance {
            package: grammar.crate_name.clone(),
        }),
    }
}

// A local binding need not have a published archive. Bind its source identity
// to sorted UTF-8 paths and lowercase file hashes instead: domain prefix, then
// path, NUL, hash, NUL for each entry. NUL-free paths make framing unambiguous.
fn vendored_source_digest(files: &BTreeMap<String, String>) -> Result<String, GrammarLockError> {
    let mut digest = rootlight_crypto::Sha256::new();
    digest.update(b"rootlight-vendored-source-v1\0");
    for (path, checksum) in files {
        if !is_source_relative_path(path) {
            return Err(GrammarLockError::InvalidSourcePath(path.clone()));
        }
        validate_sha256("vendored source", checksum)?;
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(checksum.as_bytes());
        digest.update([0]);
    }
    Ok(rootlight_encoding::HEXLOWER.encode(&digest.finalize()))
}

fn is_source_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.split('/').any(|part| {
            part.is_empty() || matches!(part, "." | "..") || part.contains(['\\', ':', '\0'])
        })
}

fn validate_direct_dependencies(metadata: &Metadata, root: &Path) -> Result<(), GrammarLockError> {
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == ADAPTER_PACKAGE)
        .ok_or(GrammarLockError::MissingAdapterPackage)?;
    validate_tree_sitter_dependencies(&package.dependencies, root)
}

fn validate_tree_sitter_dependencies(
    dependencies: &[Dependency],
    root: &Path,
) -> Result<(), GrammarLockError> {
    let expected: BTreeMap<_, _> = EXPECTED_PACKAGES
        .iter()
        .map(|(name, version, _)| (*name, (*version, None)))
        .chain(
            EXPECTED_LOCAL_PACKAGES
                .iter()
                .map(|(name, version, directory, _)| {
                    (*name, (*version, Some(root.join(directory))))
                }),
        )
        .collect();
    let observed: Vec<_> = dependencies
        .iter()
        .filter(|dependency| {
            is_tree_sitter_package(&dependency.name)
                || dependency
                    .rename
                    .as_deref()
                    .is_some_and(is_tree_sitter_package)
        })
        .collect();
    let observed_names: BTreeSet<_> = observed
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect();
    let expected_names: BTreeSet<_> = expected.keys().copied().collect();
    if observed.len() != expected.len() || observed_names != expected_names {
        return Err(GrammarLockError::DependencySet);
    }
    for (name, (version, path)) in expected {
        let dependency = observed
            .iter()
            .find(|dependency| dependency.name == name)
            .ok_or(GrammarLockError::DependencySet)?;
        validate_dependency_profile(dependency, version, path.as_deref())?;
    }
    Ok(())
}

fn is_tree_sitter_package(name: &str) -> bool {
    name == "tree-sitter"
        || name == "dekobon-tree-sitter-groovy"
        || name.starts_with("tree-sitter-")
        || name.starts_with("ts-parser-")
}

fn validate_dependency_profile(
    dependency: &Dependency,
    version: &str,
    expected_path: Option<&Path>,
) -> Result<(), GrammarLockError> {
    // The local Scheme binding keeps the public crate alias while its package
    // identity distinguishes it from the retained upstream distribution.
    let expected_rename =
        (dependency.name == "tree-sitter-scheme-native-local").then_some("tree-sitter-scheme");
    let source_matches = match expected_path {
        Some(path) => dependency.source.is_none() && dependency.path.as_deref() == Some(path),
        None => {
            dependency.path.is_none()
                && dependency
                    .source
                    .as_ref()
                    .map(ToString::to_string)
                    .as_deref()
                    == Some(CRATES_IO_SOURCE)
        }
    };
    if dependency.req != format!("={version}")
        || dependency.kind != DependencyKind::Normal
        || dependency.optional
        || dependency.uses_default_features
        || dependency.rename.as_deref() != expected_rename
        || dependency.target.is_some()
        || dependency.registry.is_some()
        || !source_matches
    {
        return Err(GrammarLockError::DependencyProfile {
            package: dependency.name.clone(),
        });
    }
    let expected_features: &[&str] = if dependency.name == "tree-sitter" {
        &["std"]
    } else {
        &[]
    };
    if dependency
        .features
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        != expected_features
    {
        return Err(GrammarLockError::DependencyProfile {
            package: dependency.name.clone(),
        });
    }
    Ok(())
}

fn validate_cargo_lock(root: &Path, manifest: &GrammarLock) -> Result<(), GrammarLockError> {
    let path = root.join(CARGO_LOCK_PATH);
    let text = fs::read_to_string(&path).map_err(|source| GrammarLockError::Read {
        path: path.clone(),
        source,
    })?;
    let lock: CargoLock =
        toml::from_str(&text).map_err(|source| GrammarLockError::Parse { path, source })?;
    validate_locked_packages(&lock, manifest)
}

fn validate_locked_packages(
    lock: &CargoLock,
    manifest: &GrammarLock,
) -> Result<(), GrammarLockError> {
    let mut expected = vec![(
        manifest.runtime.crate_name.as_str(),
        manifest.runtime.crate_version.as_str(),
        Some(manifest.runtime.crates_io_checksum.as_str()),
        false,
    )];
    for grammar in &manifest.grammars {
        validate_source_provenance(grammar)?;
        expected.push((
            grammar.crate_name.as_str(),
            grammar.crate_version.as_str(),
            grammar.crates_io_checksum.as_deref(),
            grammar.vendored.is_some(),
        ));
    }
    for (name, version, checksum, vendored) in expected {
        let entries = lock
            .package
            .iter()
            .filter(|package| package.name == name && package.version == version)
            .collect::<Vec<_>>();
        let valid = match entries.as_slice() {
            [package] if vendored => package.checksum.is_none() && package.source.is_none(),
            [package] => {
                checksum.is_some()
                    && package.checksum.as_deref() == checksum
                    && package.source.as_deref() == Some(CRATES_IO_SOURCE)
            }
            _ => false,
        };
        if !valid {
            return Err(GrammarLockError::CargoLockChecksum {
                package: name.to_owned(),
                version: version.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_vendored_sources(
    metadata: &Metadata,
    root: &Path,
    manifest: &GrammarLock,
) -> Result<(), GrammarLockError> {
    for grammar in &manifest.grammars {
        let Some(vendored) = &grammar.vendored else {
            continue;
        };
        let invalid = || GrammarLockError::PackageEvidence {
            package: grammar.crate_name.clone(),
        };
        let manifest_path = Path::new(&vendored.manifest_path);
        if manifest_path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(invalid());
        }
        let expected_manifest = root.join(manifest_path);
        let package = metadata
            .packages
            .iter()
            .find(|package| {
                package.name.as_str() == grammar.crate_name
                    && package.version == grammar.crate_version
            })
            .ok_or_else(invalid)?;
        if package.source.is_some() || package.manifest_path != expected_manifest {
            return Err(invalid());
        }
        let directory = expected_manifest.parent().ok_or_else(invalid)?;
        validate_vendored_tree(directory, grammar, vendored)?;
    }
    Ok(())
}

fn validate_vendored_tree(
    directory: &Path,
    grammar: &GrammarEvidence,
    vendored: &VendoredGrammarEvidence,
) -> Result<(), GrammarLockError> {
    validate_source_provenance(grammar)?;
    let invalid = || GrammarLockError::PackageEvidence {
        package: grammar.crate_name.clone(),
    };
    // Resolve the ABI header beside the selected parser, including nested
    // grammar profiles; a package-level header is not an interchangeable input.
    let parser_path = vendored.parser_path.as_deref().unwrap_or("src/parser.c");
    let scanner_path = vendored.scanner_path.as_deref().unwrap_or("src/scanner.c");
    let license_path = vendored.license_path.as_deref().unwrap_or("LICENSE");
    for path in [parser_path, scanner_path, license_path] {
        if !is_source_relative_path(path) {
            return Err(invalid());
        }
    }
    let header_path = Path::new(parser_path)
        .parent()
        .ok_or_else(invalid)?
        .join("tree_sitter/parser.h")
        .to_string_lossy()
        .replace('\\', "/");
    if !is_source_relative_path(&header_path)
        || !vendored.files.contains_key(parser_path)
        || !vendored.files.contains_key(&header_path)
    {
        return Err(invalid());
    }
    let mut parser_bytes = None;
    let mut header_bytes = None;
    let mut observed = BTreeSet::new();
    let mut pending = vec![directory.to_owned()];
    while let Some(path) = pending.pop() {
        let kind = fs::symlink_metadata(&path)
            .map_err(|source| GrammarLockError::Read {
                path: path.clone(),
                source,
            })?
            .file_type();
        if kind.is_symlink() || !kind.is_dir() {
            return Err(invalid());
        }
        let entries = fs::read_dir(&path).map_err(|source| GrammarLockError::Read {
            path: path.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| GrammarLockError::Read {
                path: path.clone(),
                source,
            })?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|source| GrammarLockError::Read {
                path: path.clone(),
                source,
            })?;
            if kind.is_symlink() {
                return Err(invalid());
            }
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if !kind.is_file() {
                return Err(invalid());
            }
            let relative = path
                .strip_prefix(directory)
                .map_err(|_| invalid())?
                .to_string_lossy()
                .replace('\\', "/");
            let digest = vendored.files.get(&relative).ok_or_else(invalid)?;
            validate_sha256("vendored source", digest)?;
            let bytes = fs::read(&path).map_err(|source| GrammarLockError::Read {
                path: path.clone(),
                source,
            })?;
            require_digest("vendored grammar source", &bytes, digest)?;
            if relative == parser_path {
                parser_bytes = Some(bytes);
            } else if relative == header_path {
                header_bytes = Some(bytes);
            }
            observed.insert(relative);
        }
    }
    if observed != vendored.files.keys().cloned().collect() || !observed.contains("Cargo.toml") {
        return Err(invalid());
    }
    let scanner = (grammar.scanner_sha256 != "none").then_some(&grammar.scanner_sha256);
    if vendored.files.get(parser_path) != Some(&grammar.parser_sha256)
        || vendored.files.get(scanner_path) != scanner
        || vendored.files.get(license_path) != Some(&grammar.license_sha256)
    {
        return Err(invalid());
    }
    // Use the exact bytes authenticated during the regular-file inventory;
    // reopening either path here would leave a second, unauthenticated read.
    let parser_bytes = parser_bytes.ok_or_else(invalid)?;
    let header_bytes = header_bytes.ok_or_else(invalid)?;
    let parser = std::str::from_utf8(&parser_bytes).map_err(|source| GrammarLockError::Utf8 {
        path: directory.join(parser_path),
        source,
    })?;
    let header = std::str::from_utf8(&header_bytes).map_err(|source| GrammarLockError::Utf8 {
        path: directory.join(&header_path),
        source,
    })?;
    native_table_bounds::validate(parser, header).map_err(|source| {
        GrammarLockError::NativeTableBounds {
            package: grammar.crate_name.clone(),
            parser_path: directory.join(parser_path),
            header_path: directory.join(&header_path),
            source,
        }
    })?;
    Ok(())
}

fn require_digest(
    label: &'static str,
    bytes: &[u8],
    expected: &str,
) -> Result<(), GrammarLockError> {
    let observed = sha256_hex(bytes);
    if observed == expected {
        Ok(())
    } else {
        Err(GrammarLockError::DigestMismatch {
            label,
            expected: expected.to_owned(),
            observed,
        })
    }
}

fn validate_sha256(label: &'static str, value: &str) -> Result<(), GrammarLockError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(GrammarLockError::InvalidDigest { label })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = rootlight_crypto::Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

fn require_nonempty(field: &'static str, value: &str) -> Result<(), GrammarLockError> {
    if value.trim().is_empty() {
        Err(GrammarLockError::EmptyField(field))
    } else {
        Ok(())
    }
}

fn require_nonempty_collection(
    field: &'static str,
    values: &[String],
) -> Result<(), GrammarLockError> {
    if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
        Err(GrammarLockError::EmptyField(field))
    } else {
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrammarLock {
    schema_version: String,
    runtime: RuntimeEvidence,
    grammars: Vec<GrammarEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeEvidence {
    crate_name: String,
    crate_version: String,
    crates_io_checksum: String,
    repository: String,
    tag: String,
    commit: String,
    abi_minimum: usize,
    abi_current: usize,
    license: String,
    license_source: String,
    license_sha256: String,
    msrv: String,
    offline_behavior: String,
    capabilities: Vec<String>,
    parser_limits: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrammarEvidence {
    language: String,
    crate_name: String,
    crate_version: String,
    crates_io_checksum: Option<String>,
    vendored_source_sha256: Option<String>,
    repository: String,
    tag: String,
    commit: String,
    parser_sha256: String,
    scanner_sha256: String,
    abi: usize,
    analysis_tier: String,
    license: String,
    license_source: String,
    license_sha256: String,
    modifications: String,
    test_corpus: String,
    msrv: String,
    offline_behavior: String,
    capabilities: Vec<String>,
    semantic_depth: String,
    audit_notes: Vec<String>,
    #[serde(default)]
    vendored: Option<VendoredGrammarEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VendoredGrammarEvidence {
    manifest_path: String,
    #[serde(default)]
    parser_path: Option<String>,
    #[serde(default)]
    scanner_path: Option<String>,
    #[serde(default)]
    license_path: Option<String>,
    files: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct CargoLock {
    package: Vec<LockedPackage>,
}

#[derive(Debug, Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
    checksum: Option<String>,
    source: Option<String>,
}

/// Failure to verify the audited grammar dependency lock.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GrammarLockError {
    #[error("failed to read grammar policy at {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("grammar policy at {} is not UTF-8", path.display())]
    Utf8 {
        path: PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("failed to parse grammar policy at {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("unsupported grammar lock schema {0}")]
    UnsupportedVersion(String),
    #[error("grammar lock digest for {label} differs: expected {expected}, observed {observed}")]
    DigestMismatch {
        label: &'static str,
        expected: String,
        observed: String,
    },
    #[error("{label} is not a lowercase SHA-256 digest")]
    InvalidDigest { label: &'static str },
    #[error("grammar lock field {0} must not be empty")]
    EmptyField(&'static str),
    #[error("grammar lock contains {0} grammars instead of 79")]
    GrammarCount(usize),
    #[error("grammar lock repeats language {0}")]
    DuplicateLanguage(String),
    #[error("grammar lock does not contain the exact audited language set")]
    LanguageSet,
    #[error("runtime evidence differs from the audited Tree-sitter release")]
    RuntimeEvidence,
    #[error("package evidence differs for {package}")]
    PackageEvidence { package: String },
    #[error("grammar {package} requires exactly one archive or vendored source identity")]
    SourceProvenance { package: String },
    #[error("vendored source path is not a canonical relative path: {0:?}")]
    InvalidSourcePath(String),
    #[error("grammar ABI {abi} is unsupported for {language}")]
    GrammarAbi { language: String, abi: usize },
    #[error("native table ABI validation failed for {package}: parser {}, header {}", parser_path.display(), header_path.display())]
    NativeTableBounds {
        package: String,
        parser_path: PathBuf,
        header_path: PathBuf,
        #[source]
        source: native_table_bounds::NativeTableBoundsError,
    },
    #[error("grammar {language} must remain at structural analysis tier D")]
    InvalidAnalysisTier { language: String },
    #[error("grammar lock omits the {0} audit caveat")]
    MissingAuditCaveat(&'static str),
    #[error("workspace metadata omits rootlight-adapter-treesitter")]
    MissingAdapterPackage,
    #[error("Tree-sitter direct dependency set differs from the grammar lock")]
    DependencySet,
    #[error("Tree-sitter dependency profile differs for {package}")]
    DependencyProfile { package: String },
    #[error("Cargo.lock checksum differs for {package}@{version}")]
    CargoLockChecksum { package: String, version: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Git stores files; authenticated archive inventories retain empty directories.
    fn archived_directory_materialized(
        path: &Path,
        member_path: &str,
        members: &[serde_json::Value],
    ) -> bool {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return false;
                    }
                }
                metadata.is_dir() && !metadata.file_type().is_symlink()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                !members.iter().any(|member| {
                    let relative = member["path"].as_str().expect("archive member path");
                    !member["directory"].as_bool().expect("archive member kind")
                        && (member_path.is_empty()
                            || relative == member_path
                            || relative
                                .strip_prefix(member_path)
                                .is_some_and(|suffix| suffix.starts_with('/')))
                })
            }
            Err(_) => false,
        }
    }

    #[test]
    fn archived_empty_directories_do_not_require_git_inodes() {
        let root = tempfile::tempdir().unwrap();
        let members = json!([
            {"path": "", "directory": true},
            {"path": "used/nested/item", "directory": false},
            {"path": "empty/nested", "directory": true},
            {"path": "emptyish/item", "directory": false}
        ]);
        let members = members.as_array().unwrap();
        let empty = root.path().join("empty");
        assert!(archived_directory_materialized(&empty, "empty", members));
        for member_path in ["", "used", "used/nested", "emptyish"] {
            assert!(!archived_directory_materialized(
                &root.path().join("absent"),
                member_path,
                members
            ));
        }
        fs::write(&empty, b"not a directory").unwrap();
        assert!(!archived_directory_materialized(&empty, "empty", members));
        let present = root.path().join("present");
        fs::create_dir(&present).unwrap();
        assert!(archived_directory_materialized(&present, "empty", members));
    }

    const FIXTURE_PARSER: &[u8] =
        b"static const uint16_t table[] = {ACTIONS(7), STATE(9)}; SHIFT(9), SHIFT_REPEAT(9)";
    const FIXTURE_HEADER: &[u8] = b"typedef uint16_t TSStateId; struct TSLanguage { const uint16_t *parse_table; const uint16_t *small_parse_table; };";

    #[test]
    fn checked_in_manifest_matches_the_reviewed_runtime_and_grammar_set() {
        require_digest(
            GRAMMAR_LOCK_PATH,
            include_bytes!("../../adapters/grammars.lock"),
            GRAMMAR_LOCK_SHA256,
        )
        .expect("grammar lock digest matches the reviewed pin");
        let manifest: GrammarLock = toml::from_str(include_str!("../../adapters/grammars.lock"))
            .expect("grammar lock parses");
        validate_manifest(&manifest).expect("reviewed runtime and grammar identities agree");
    }

    #[test]
    fn coq_retains_authored_sources_and_distinguishes_the_external_license_reference() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let directory = root.join("third_party/tree-sitter-rocq-0.2.0");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "coq")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        validate_cargo_lock(&root, &manifest).unwrap();
        validate_vendored_tree(&directory, grammar, sources).unwrap();
        let inventory: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("upstream/retained-source-inventory.json")).unwrap(),
        )
        .unwrap();
        let members = inventory["files"].as_array().unwrap();
        assert_eq!(members.len(), 91);
        let mut paths = BTreeSet::new();
        for member in members {
            assert!(paths.insert(member["path"].as_str().unwrap()));
            assert_eq!(member["authored_bytes_unchanged"], true);
            assert!(member["upstream_mode"].as_str().is_some());
            assert!(member["executable_intent"].as_bool().is_some());
            let retained = member["retained_path"].as_str().unwrap();
            let bytes = fs::read(directory.join(retained)).unwrap();
            assert_eq!(
                u64::try_from(bytes.len()).unwrap(),
                member["bytes"].as_u64().unwrap()
            );
            assert_eq!(sha256_hex(&bytes), member["sha256"].as_str().unwrap());
            assert_eq!(sources.files.get(retained), Some(&sha256_hex(&bytes)));
        }
        let origin: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("upstream/source-origin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(origin["authored_full_license_notice_present"], false);
        assert_eq!(
            origin["external_license_reference"]["upstream_authored_notice"],
            false
        );
        assert_eq!(
            origin["external_license_reference"]["copyright_placeholders_filled"],
            false
        );
        assert_eq!(origin["upstream_code_executed_during_import"], false);
        let archive = origin["archive"]["path"].as_str().unwrap();
        let archive_bytes = fs::read(directory.join("upstream").join(archive)).unwrap();
        assert_eq!(
            sha256_hex(&archive_bytes),
            origin["archive"]["sha256"].as_str().unwrap()
        );
        let declarations: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join(COQ_LICENSE_DECLARATIONS_PATH)).unwrap())
                .unwrap();
        assert_eq!(
            declarations["license_declaration_locations"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(declarations["authored_notice_overlay_recovered"], false);
        assert_eq!(
            declarations["spdx_is_external_reference_not_authored_notice"],
            true
        );
        let reference: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("upstream/spdx-MIT-reference.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(reference["licenseId"], "MIT");
        let reference_text =
            fs::read_to_string(directory.join("LICENSE-MIT-REFERENCE.txt")).unwrap();
        // The separately locked text file retains CRLF; SPDX JSON retains LF.
        assert_eq!(
            reference["licenseText"].as_str().unwrap(),
            reference_text.replace("\r\n", "\n"),
        );
        let regeneration: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("patches/grammar-regeneration.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(regeneration["independent_fresh_directory_replays"], 2);
        assert_eq!(regeneration["generated_C_or_schema_hand_edited"], false);
        assert_eq!(
            regeneration["executed_during_compilation_or_packaging"],
            false
        );
        for item in regeneration["local_javascript_input_closure"]
            .as_array()
            .unwrap()
            .iter()
            .chain(regeneration["outputs"].as_array().unwrap())
        {
            let bytes = fs::read(directory.join(item["path"].as_str().unwrap())).unwrap();
            assert_eq!(
                u64::try_from(bytes.len()).unwrap(),
                item["bytes"].as_u64().unwrap()
            );
            assert_eq!(sha256_hex(&bytes), item["sha256"].as_str().unwrap());
        }
    }

    #[test]
    fn coq_license_provenance_rejects_substitution_with_the_external_reference() {
        let mut manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter_mut()
            .find(|grammar| grammar.language == "coq")
            .unwrap();
        grammar.license_source =
            "third_party/tree-sitter-rocq-0.2.0/LICENSE-MIT-REFERENCE.txt".to_owned();
        assert!(matches!(
            validate_grammar(grammar),
            Err(GrammarLockError::MissingAuditCaveat("coq"))
        ));
    }

    #[test]
    fn eiffel_retains_exact_authored_inventory_and_scanner_inputs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let directory = root.join("third_party/tree-sitter-eiffel-1.0.0");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "eiffel")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        assert_eq!(sources.files.len(), 96);
        validate_vendored_tree(&directory, grammar, sources).unwrap();
        assert_eq!(
            grammar.scanner_sha256,
            sha256_hex(&fs::read(directory.join("src/scanner.c")).unwrap())
        );
        let acquisition: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("upstream/acquisition-inventory.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            acquisition["upstream_revision"],
            "ec098b67b98b7f5da10f8d66bdb6c52fc91341fc"
        );
        assert_eq!(
            acquisition["upstream_git_tree"],
            "b9cf17dff75b9582b74849df0d7b7004843e2cda"
        );
        assert_eq!(
            acquisition["authored_git_blob_inventory_authenticated"],
            true
        );
        assert!(acquisition["published_inventory"].is_null());
        assert_eq!(acquisition["archive_modes_applied"], false);
        assert_eq!(acquisition["upstream_code_executed"], false);
        let members = acquisition["authored_inventory"].as_array().unwrap();
        assert_eq!(members.len(), 71);
        let mut originals = BTreeSet::new();
        for member in members {
            let original = member["path"].as_str().unwrap();
            assert!(originals.insert(original));
            let relative = match original {
                ".gitignore" => ".gitignore.upstream",
                ".gitattributes" => ".gitattributes.upstream",
                path => path,
            };
            assert_eq!(member["retained_path"], relative);
            let raw = fs::read(directory.join("upstream/authored-files").join(relative)).unwrap();
            assert_eq!(
                u64::try_from(raw.len()).unwrap(),
                member["bytes"].as_u64().unwrap()
            );
            assert_eq!(sha256_hex(&raw), member["sha256"].as_str().unwrap());
            assert_eq!(
                member["executable_intent"].as_bool().unwrap(),
                member["archive_mode"].as_u64().unwrap() & 0o111 != 0
            );
        }
        let inventory: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("SOURCE-INVENTORY.json")).unwrap())
                .unwrap();
        let mut payload = BTreeMap::new();
        for member in inventory["files"].as_array().unwrap() {
            let path = member["path"].as_str().unwrap();
            let raw = fs::read(directory.join(path)).unwrap();
            assert_eq!(
                u64::try_from(raw.len()).unwrap(),
                member["bytes"].as_u64().unwrap()
            );
            assert_eq!(sha256_hex(&raw), member["sha256"].as_str().unwrap());
            assert!(payload.insert(path.to_owned(), sha256_hex(&raw)).is_none());
        }
        let mut locked_payload = sources.files.clone();
        locked_payload.remove("SOURCE-INVENTORY.json").unwrap();
        locked_payload.remove("SOURCE-PROVENANCE.json").unwrap();
        assert_eq!(payload, locked_payload);
        let provenance: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("SOURCE-PROVENANCE.json")).unwrap())
                .unwrap();
        assert_eq!(
            provenance["inventory"]["sha256"],
            sha256_hex(&fs::read(directory.join("SOURCE-INVENTORY.json")).unwrap())
        );
        assert_eq!(
            provenance["preferred_inputs"]["generated_files_hand_edited"],
            false
        );
        assert_eq!(
            provenance["preferred_inputs"]["parser_literal_upstream"],
            false
        );
        assert_eq!(provenance["preferred_inputs"]["external_token_count"], 1);
        assert_eq!(provenance["preferred_inputs"]["scanner"], "src/scanner.c");
        assert_eq!(
            provenance["build"]["inputs"],
            serde_json::json!([
                "src/parser.c",
                "src/scanner.c",
                "src/operator_symbols.h",
                "src/tree_sitter/parser.h"
            ])
        );
        assert_eq!(provenance["build"]["acquisition_or_generation"], false);
        assert_eq!(provenance["build"]["uses_upstream_lifecycle_hooks"], false);
        for path in ["LICENSE", "tree-sitter.json"] {
            assert_eq!(
                fs::read(directory.join(path)).unwrap(),
                fs::read(directory.join("upstream/authored-files").join(path)).unwrap()
            );
        }
        let regeneration: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("patches/grammar-regeneration.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(regeneration["generator"]["version"], "0.26.8");
        assert_eq!(regeneration["generator"]["javascript_runtime"], "native");
        assert_eq!(regeneration["independent_fresh_directory_replays"], 2);
        assert_eq!(regeneration["generated_files_hand_edited"], false);
        assert_eq!(regeneration["original_upstream_inputs_preserved"], true);
        assert_eq!(
            regeneration["executed_during_compilation_or_packaging"],
            false
        );
        assert_eq!(
            regeneration["command"],
            serde_json::json!([
                "tree-sitter",
                "generate",
                "--abi",
                "15",
                "--js-runtime",
                "native"
            ])
        );
        let outputs = regeneration["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 6);
        for member in regeneration["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .chain(outputs)
        {
            let raw = fs::read(directory.join(member["path"].as_str().unwrap())).unwrap();
            assert_eq!(
                u64::try_from(raw.len()).unwrap(),
                member["bytes"].as_u64().unwrap()
            );
            assert_eq!(sha256_hex(&raw), member["sha256"].as_str().unwrap());
        }
        let parser = fs::read_to_string(directory.join("src/parser.c")).unwrap();
        let header = fs::read_to_string(directory.join("src/tree_sitter/parser.h")).unwrap();
        let bounds = native_table_bounds::validate(&parser, &header).unwrap();
        assert!(bounds.action_references > 0);
        assert!(bounds.state_references > 0);
        assert_ne!(
            fs::read(directory.join("src/parser.c")).unwrap(),
            fs::read(directory.join("upstream/authored-files/src/parser.c")).unwrap()
        );
        let symbols: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("patches/operator-symbols-regeneration.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(symbols["unicode_version"], "16.0.0");
        assert_eq!(symbols["independent_fresh_directory_replays"], 2);
        assert_eq!(symbols["executed_during_compilation_or_packaging"], false);
        for member in symbols["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .chain(symbols["outputs"].as_array().unwrap())
        {
            let raw = fs::read(directory.join(member["path"].as_str().unwrap())).unwrap();
            assert_eq!(raw.len() as u64, member["bytes"].as_u64().unwrap());
            assert_eq!(sha256_hex(&raw), member["sha256"].as_str().unwrap());
        }
        for (retained, original) in [
            (
                "regex-syntax-general-category.rs",
                "src/unicode_tables/general_category.rs",
            ),
            ("LICENSE-MIT", "LICENSE-MIT"),
            ("LICENSE-APACHE", "LICENSE-APACHE"),
        ] {
            assert_eq!(
                fs::read(directory.join("patches/unicode").join(retained)).unwrap(),
                fs::read(
                    root.join("source-inputs/cargo/regex-syntax-0.8.11")
                        .join(original)
                )
                .unwrap()
            );
        }
        validate_local_license(
            &root,
            "adapters/licenses/tree-sitter-eiffel-1.0.0-LICENSE",
            grammar.license_sha256.as_str(),
        )
        .unwrap();
    }

    #[test]
    fn hdl_retains_distinct_authored_and_published_sources_with_static_native_inputs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let directory = root.join("third_party/tree-sitter-systemverilog-0.4.1");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        for language in ["verilog", "systemverilog"] {
            let grammar = manifest
                .grammars
                .iter()
                .find(|grammar| grammar.language == language)
                .unwrap();
            let sources = grammar.vendored.as_ref().unwrap();
            assert_eq!(sources.files.len(), 117);
            assert_eq!(grammar.abi, 15);
            assert_eq!(grammar.scanner_sha256, "none");
            assert_eq!(grammar.commit, "3390da8c76a976b6f47d19667accc43e81a1483b");
            assert_eq!(grammar.parser_sha256, sources.files["src/parser.c"]);
            assert!(
                sources
                    .files
                    .contains_key("upstream/tree-sitter-systemverilog-published.tar.gz")
            );
            assert!(
                sources
                    .files
                    .contains_key("upstream/tree-sitter-systemverilog-authored.tar.gz")
            );
            validate_local_license(
                &root,
                "adapters/licenses/tree-sitter-systemverilog-0.4.1-LICENSE",
                &grammar.license_sha256,
            )
            .unwrap();
        }
        let inventory: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("SOURCE-INVENTORY.json")).unwrap())
                .unwrap();
        for row in inventory["files"].as_array().unwrap() {
            let path = row["path"].as_str().unwrap();
            let bytes = fs::read(directory.join(path)).unwrap();
            assert_eq!(bytes.len() as u64, row["bytes"].as_u64().unwrap(), "{path}");
            require_digest(
                "HDL source inventory",
                &bytes,
                row["sha256"].as_str().unwrap(),
            )
            .unwrap();
        }
        let provenance: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("SOURCE-PROVENANCE.json")).unwrap())
                .unwrap();
        assert_eq!(provenance["upstream"]["preserved_authored_files"], 47);
        assert_eq!(provenance["upstream"]["preserved_published_files"], 47);
        assert_eq!(
            provenance["upstream"]["differing_paths"],
            serde_json::json!(["src/tree_sitter/array.h"])
        );
        assert_eq!(provenance["foreign_tests"]["retained_in_package"], false);
        assert_eq!(
            provenance["foreign_tests"]["blanket_redistribution_clearance"],
            false
        );
        assert!(
            fs::read_to_string(directory.join("bindings/rust/lib.rs"))
                .unwrap()
                .contains("LANGUAGE_SYSTEMVERILOG")
        );
        assert!(!directory.join("src/scanner.c").exists());
    }

    #[test]
    fn v_retains_unpublished_binding_and_complete_authored_native_inputs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let directory = root.join("third_party/tree-sitter-v-0.0.6");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "v")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        assert_eq!(grammar.abi, 15);
        assert_eq!(grammar.scanner_sha256, "none");
        assert_eq!(grammar.commit, "925d4570d1668746762a2cdf0ecb9a25be704a67");
        assert_eq!(grammar.parser_sha256, sources.files["src/parser.c"]);
        for path in [
            "upstream/v-analyzer-authored.tar.gz",
            "upstream/tree-sitter-core-authored.tar.gz",
            "bindings/rust/build.rs",
            "bindings/rust/lib.rs",
            "src/tree_sitter/parser.h",
            "patches/grammar.patch",
            "patches/regeneration.json",
        ] {
            assert!(sources.files.contains_key(path), "{path}");
        }
        let inventory: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("SOURCE-INVENTORY.json")).unwrap())
                .unwrap();
        for row in inventory["files"].as_array().unwrap() {
            let path = row["path"].as_str().unwrap();
            let bytes = fs::read(directory.join(path)).unwrap();
            assert_eq!(bytes.len() as u64, row["bytes"].as_u64().unwrap(), "{path}");
            require_digest(
                "V source inventory",
                &bytes,
                row["sha256"].as_str().unwrap(),
            )
            .unwrap();
        }
        let provenance: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("SOURCE-PROVENANCE.json")).unwrap())
                .unwrap();
        assert_eq!(provenance["package"]["unpublished_local_binding"], true);
        assert_eq!(provenance["package"]["published_release_claimed"], false);
        assert_eq!(provenance["upstream"]["preserved_files"], 527);
        assert_eq!(provenance["inactive_gitlink"]["preserved_files"], 525);
        assert_eq!(
            provenance["inactive_gitlink"]["used_by_local_binding"],
            false
        );
        assert_eq!(provenance["build"]["acquisition_or_generation"], false);
        assert!(!directory.join("src/scanner.c").exists());
        validate_local_license(
            &root,
            "adapters/licenses/tree-sitter-v-0.0.6-LICENSE",
            &grammar.license_sha256,
        )
        .unwrap();
    }

    #[test]
    fn julia_retains_exact_original_inventories_and_native_inputs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let directory = root.join("third_party/tree-sitter-julia-0.25.0");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "julia")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        assert_eq!(sources.files.len(), 142);
        validate_vendored_tree(&directory, grammar, sources).unwrap();
        let acquisition: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("upstream/acquisition-inventory.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            acquisition["upstream_revision"],
            "e0f9dcd180fdcfcfa8d79a3531e11d99e79321d3"
        );
        assert_eq!(
            acquisition["upstream_git_tree"],
            "748cd55f16c836ad81cd3a4b50f603ed11665704"
        );
        assert_eq!(
            acquisition["authored_git_blob_inventory_authenticated"],
            true
        );
        assert_eq!(acquisition["authored_published_bytes_identical"], true);
        assert_eq!(acquisition["archive_modes_applied"], false);
        assert_eq!(acquisition["upstream_code_executed"], false);
        assert_eq!(acquisition["signature_verification"]["verified"], false);
        for (label, retained) in [
            ("authored_inventory", "authored-files"),
            ("published_inventory", "published-files"),
        ] {
            let members = acquisition[label].as_array().unwrap();
            assert_eq!(members.len(), 61);
            let mut unique = BTreeSet::new();
            for member in members {
                let original = member["path"].as_str().unwrap();
                assert!(unique.insert(original));
                let relative = match original {
                    ".gitignore" => ".gitignore.upstream",
                    ".gitattributes" => ".gitattributes.upstream",
                    path => path,
                };
                assert_eq!(member["retained_path"], relative);
                let raw =
                    fs::read(directory.join("upstream").join(retained).join(relative)).unwrap();
                assert_eq!(
                    u64::try_from(raw.len()).unwrap(),
                    member["bytes"].as_u64().unwrap()
                );
                assert_eq!(sha256_hex(&raw), member["sha256"].as_str().unwrap());
                assert_eq!(
                    member["executable_intent"].as_bool().unwrap(),
                    member["archive_mode"].as_u64().unwrap() & 0o111 != 0
                );
                assert_eq!(
                    raw,
                    fs::read(directory.join("upstream/authored-files").join(relative)).unwrap()
                );
            }
        }
        let inventory: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("SOURCE-INVENTORY.json")).unwrap())
                .unwrap();
        let mut payload = BTreeMap::new();
        for member in inventory["files"].as_array().unwrap() {
            let path = member["path"].as_str().unwrap();
            let raw = fs::read(directory.join(path)).unwrap();
            assert_eq!(
                u64::try_from(raw.len()).unwrap(),
                member["bytes"].as_u64().unwrap()
            );
            assert_eq!(sha256_hex(&raw), member["sha256"].as_str().unwrap());
            assert!(payload.insert(path.to_owned(), sha256_hex(&raw)).is_none());
        }
        let mut locked_payload = sources.files.clone();
        locked_payload.remove("SOURCE-INVENTORY.json").unwrap();
        locked_payload.remove("SOURCE-PROVENANCE.json").unwrap();
        assert_eq!(payload, locked_payload);
        let provenance: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("SOURCE-PROVENANCE.json")).unwrap())
                .unwrap();
        assert_eq!(
            provenance["inventory"]["sha256"],
            sha256_hex(&fs::read(directory.join("SOURCE-INVENTORY.json")).unwrap())
        );
        assert_eq!(
            provenance["preferred_inputs"]["generated_files_hand_edited"],
            false
        );
        assert_eq!(
            provenance["preferred_inputs"]["parser_and_scanner_literal_upstream"],
            true
        );
        assert_eq!(provenance["build"]["acquisition_or_generation"], false);
        assert_eq!(provenance["build"]["uses_upstream_lifecycle_hooks"], false);
        assert_eq!(provenance["qualification"]["installed_mcp_verified"], false);
        assert_eq!(
            provenance["qualification"]["empty_cache_independent_network_denial_verified"],
            false
        );
        for path in [
            "LICENSE",
            "grammar.js",
            "tree-sitter.json",
            "src/parser.c",
            "src/scanner.c",
            "src/grammar.json",
            "src/node-types.json",
            "src/tree_sitter/parser.h",
            "src/tree_sitter/alloc.h",
            "src/tree_sitter/array.h",
        ] {
            assert_eq!(
                fs::read(directory.join(path)).unwrap(),
                fs::read(directory.join("upstream/authored-files").join(path)).unwrap()
            );
        }
        let parser = fs::read_to_string(directory.join("src/parser.c")).unwrap();
        let header = fs::read_to_string(directory.join("src/tree_sitter/parser.h")).unwrap();
        let bounds = native_table_bounds::validate(&parser, &header).unwrap();
        assert!(bounds.action_references > 0);
        assert!(bounds.state_references > 0);
        validate_local_license(
            &root,
            "adapters/licenses/tree-sitter-julia-0.25.0-LICENSE",
            grammar.license_sha256.as_str(),
        )
        .unwrap();
    }

    #[test]
    fn cmake_retains_exact_original_inventories_and_reviewed_native_inputs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let directory = root.join("third_party/tree-sitter-cmake-0.7.5");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "cmake")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        assert_eq!(sources.files.len(), 116);
        validate_vendored_tree(&directory, grammar, sources).unwrap();
        let acquisition: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("upstream/acquisition-inventory.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            acquisition["upstream_revision"],
            "e997bd0b275ca525ce9befecedf5299031183661"
        );
        assert_eq!(acquisition["archive_modes_applied"], false);
        assert_eq!(acquisition["upstream_code_executed"], false);
        assert_eq!(acquisition["signature_verification"]["verified"], false);
        for (label, retained, expected) in [
            ("authored_inventory", "authored-files", 67),
            ("published_inventory", "published-files", 21),
        ] {
            let members = acquisition[label].as_array().unwrap();
            assert_eq!(members.len(), expected);
            let mut unique = BTreeSet::new();
            for member in members {
                let original = member["path"].as_str().unwrap();
                assert!(unique.insert(original));
                let relative = match original {
                    ".gitignore" => ".gitignore.upstream",
                    ".gitattributes" => ".gitattributes.upstream",
                    path => path,
                };
                let raw =
                    fs::read(directory.join("upstream").join(retained).join(relative)).unwrap();
                assert_eq!(
                    u64::try_from(raw.len()).unwrap(),
                    member["bytes"].as_u64().unwrap()
                );
                assert_eq!(sha256_hex(&raw), member["sha256"].as_str().unwrap());
                assert!(member["archive_mode"].as_u64().is_some());
            }
        }
        let inventory: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("SOURCE-INVENTORY.json")).unwrap())
                .unwrap();
        let mut payload = BTreeMap::new();
        for member in inventory["files"].as_array().unwrap() {
            let path = member["path"].as_str().unwrap();
            let raw = fs::read(directory.join(path)).unwrap();
            assert_eq!(
                u64::try_from(raw.len()).unwrap(),
                member["bytes"].as_u64().unwrap()
            );
            assert_eq!(sha256_hex(&raw), member["sha256"].as_str().unwrap());
            assert!(payload.insert(path.to_owned(), sha256_hex(&raw)).is_none());
        }
        let mut locked_payload = sources.files.clone();
        locked_payload.remove("SOURCE-INVENTORY.json").unwrap();
        locked_payload.remove("SOURCE-PROVENANCE.json").unwrap();
        assert_eq!(payload, locked_payload);
        let provenance: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("SOURCE-PROVENANCE.json")).unwrap())
                .unwrap();
        assert_eq!(
            provenance["preferred_inputs"]["generated_files_hand_edited"],
            false
        );
        assert_eq!(provenance["build"]["acquisition_or_generation"], false);
        assert_eq!(provenance["build"]["uses_upstream_lifecycle_hooks"], false);
        assert_eq!(provenance["qualification"]["installed_mcp_verified"], false);
        assert_eq!(
            provenance["qualification"]["empty_cache_independent_network_denial_verified"],
            false
        );
        let parser = fs::read_to_string(directory.join("src/parser.c")).unwrap();
        let header = fs::read_to_string(directory.join("src/tree_sitter/parser.h")).unwrap();
        let bounds = native_table_bounds::validate(&parser, &header).unwrap();
        assert!(bounds.action_references > 0);
        assert!(bounds.state_references > 0);
        assert_ne!(
            fs::read(directory.join("src/parser.c")).unwrap(),
            fs::read(directory.join("upstream/authored-files/src/parser.c")).unwrap()
        );
        assert_eq!(
            provenance["preferred_inputs"]["parser_and_schema_literal_upstream"],
            false
        );
        let regeneration: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("patches/grammar-regeneration.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(regeneration["generator"]["version"], "0.26.8");
        assert_eq!(regeneration["generator"]["javascript_runtime"], "native");
        assert_eq!(regeneration["independent_fresh_directory_replays"], 2);
        assert_eq!(regeneration["generated_files_hand_edited"], false);
        assert_eq!(
            regeneration["executed_during_compilation_or_packaging"],
            false
        );
        assert_eq!(
            regeneration["command"],
            serde_json::json!([
                "tree-sitter",
                "generate",
                "--abi",
                "14",
                "--js-runtime",
                "native"
            ])
        );
        let outputs = regeneration["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 6);
        for item in regeneration["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .chain(outputs)
        {
            let raw = fs::read(directory.join(item["path"].as_str().unwrap())).unwrap();
            assert_eq!(
                u64::try_from(raw.len()).unwrap(),
                item["bytes"].as_u64().unwrap()
            );
            assert_eq!(sha256_hex(&raw), item["sha256"].as_str().unwrap());
        }
        validate_local_license(
            &root,
            "adapters/licenses/tree-sitter-cmake-0.7.5-LICENSE",
            grammar.license_sha256.as_str(),
        )
        .unwrap();
    }

    #[test]
    fn checked_in_common_lisp_vendor_binds_the_primary_notice() {
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "common-lisp")
            .expect("Common Lisp source evidence is present");
        let vendored = grammar.vendored.as_ref().expect("local source evidence");
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(&vendored.manifest_path)
            .parent()
            .unwrap()
            .to_owned();
        validate_vendored_tree(&directory, grammar, vendored)
            .expect("all source pins and the exact primary notice agree");
        assert_eq!(vendored.license_path.as_deref(), Some("LICENSE.md"));
    }

    #[test]
    fn checked_in_scheme_vendor_binds_the_primary_notice() {
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "scheme")
            .expect("Scheme source evidence is present");
        let vendored = grammar.vendored.as_ref().expect("local source evidence");
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(&vendored.manifest_path)
            .parent()
            .unwrap()
            .to_owned();
        validate_vendored_tree(&directory, grammar, vendored)
            .expect("all source pins and the exact primary notice agree");
        assert_eq!(vendored.license_path.as_deref(), Some("upstream/LICENSE"));
        assert_eq!(
            vendored.parser_path.as_deref(),
            Some("upstream/src/parser.c")
        );
        assert_eq!(grammar.scanner_sha256, "none");
    }

    #[test]
    fn ocaml_profiles_share_complete_sources_and_distinct_native_parsers() {
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let directory = root.join("third_party/tree-sitter-ocaml-0.25.0");
        let profiles: Vec<_> = manifest
            .grammars
            .iter()
            .filter(|grammar| grammar.crate_name == "tree-sitter-ocaml")
            .collect();
        assert_eq!(profiles.len(), 2);
        let expected_profiles = BTreeMap::from([
            ("ocaml", "grammars/ocaml/src"),
            ("ocaml-interface", "grammars/interface/src"),
        ]);
        assert_eq!(
            profiles
                .iter()
                .map(|grammar| grammar.language.as_str())
                .collect::<BTreeSet<_>>(),
            expected_profiles.keys().copied().collect()
        );
        let sources = profiles[0].vendored.as_ref().unwrap();
        assert_eq!(sources.files.len(), 121);
        for grammar in &profiles {
            let vendored = grammar.vendored.as_ref().unwrap();
            let profile = expected_profiles[grammar.language.as_str()];
            assert_eq!(vendored.files, sources.files);
            assert_eq!(vendored.parser_path, Some(format!("{profile}/parser.c")));
            assert_eq!(vendored.scanner_path, Some(format!("{profile}/scanner.c")));
            assert_eq!(vendored.license_path.as_deref(), Some("LICENSE"));
            assert!(grammar.crates_io_checksum.is_none());
            assert_eq!(grammar.crate_version, "0.25.0");
            assert_eq!(grammar.abi, 15);
            assert_eq!(grammar.license, "MIT");
            assert_eq!(
                grammar.vendored_source_sha256.as_deref(),
                Some("544e58ed05a66a74ef293db0c2825003d94c6e8426c38aff5603e3743c183f10")
            );
            validate_vendored_tree(&directory, grammar, vendored).unwrap();
        }
        let binding: toml::Value = toml::from_str(include_str!(
            "../../third_party/tree-sitter-ocaml-0.25.0/Cargo.toml"
        ))
        .unwrap();
        assert_eq!(
            binding["package"]["name"].as_str(),
            Some("tree-sitter-ocaml")
        );
        assert_eq!(binding["package"]["publish"].as_bool(), Some(false));
        assert_eq!(binding["package"]["rust-version"].as_str(), Some("1.90"));
        assert_eq!(
            binding["package"]["build"].as_str(),
            Some("bindings/rust/build.rs")
        );
        assert_eq!(
            binding["lib"]["path"].as_str(),
            Some("bindings/rust/lib.rs")
        );
        assert_eq!(
            binding["dependencies"]["tree-sitter-language"].as_str(),
            Some("=0.1.7")
        );
        assert_eq!(
            binding["build-dependencies"]["cc"].as_str(),
            Some("=1.2.67")
        );
    }

    #[test]
    fn ocaml_vendor_preserves_archive_inputs_and_declared_scanner_repair() {
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "ocaml")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        let directory =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../third_party/tree-sitter-ocaml-0.25.0");
        validate_vendored_tree(&directory, grammar, sources).unwrap();
        let inventory: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-ocaml-0.25.0/SOURCE-INVENTORY.json"
        ))
        .unwrap();
        assert_eq!(inventory["schema"], "rootlight.ocaml-source-inventory/1");
        assert_eq!(inventory["original_file_count"], 115);
        assert_eq!(inventory["original_file_bytes"], 37_241_337);
        assert_eq!(
            inventory["original_directories"].as_array().unwrap().len(),
            52
        );
        assert_eq!(inventory["payload_file_count"], 119);
        assert_eq!(inventory["complete_package_file_count"], 121);
        let payloads = inventory["payload_files"].as_array().unwrap();
        assert_eq!(payloads.len(), 119);
        let mut paths = BTreeSet::new();
        for payload in payloads {
            let path = payload["path"].as_str().unwrap();
            assert!(is_source_relative_path(path));
            assert!(paths.insert(path));
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                payload["sha256"].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                payload["bytes"].as_u64().unwrap()
            );
        }
        paths.extend(["SOURCE-INVENTORY.json", "SOURCE-PROVENANCE.json"]);
        assert_eq!(paths, sources.files.keys().map(String::as_str).collect());
        let remaps = BTreeMap::from([
            (".gitattributes", ".gitattributes.upstream"),
            (".gitignore", ".gitignore.upstream"),
            ("Cargo.toml", "Cargo.toml.upstream"),
            ("common/scanner.h", "common/scanner.h.orig"),
        ]);
        let originals = inventory["original_files"].as_array().unwrap();
        assert_eq!(originals.len(), 115);
        let mut original_paths = BTreeSet::new();
        let mut original_bytes = 0_u64;
        let mut executable_paths = BTreeSet::new();
        for original in originals {
            let member = original["path"].as_str().unwrap();
            assert!(is_source_relative_path(member));
            let retained = remaps.get(member).copied().unwrap_or(member);
            assert_eq!(original["retained_path"], retained);
            assert!(original_paths.insert(retained));
            assert_eq!(
                sources.files.get(retained).map(String::as_str),
                original["sha256"].as_str()
            );
            let bytes = original["bytes"].as_u64().unwrap();
            original_bytes = original_bytes.checked_add(bytes).unwrap();
            assert_eq!(fs::metadata(directory.join(retained)).unwrap().len(), bytes);
            let executable = original["executable"].as_bool().unwrap();
            assert_eq!(original["archive_mode"], if executable { 493 } else { 420 });
            if executable {
                executable_paths.insert(retained);
            }
        }
        assert_eq!(original_bytes, 37_241_337);
        assert_eq!(executable_paths, BTreeSet::from(["test/parse-examples.sh"]));
        assert_eq!(
            original_paths,
            payloads
                .iter()
                .filter(|payload| payload["original_archive_member"].as_bool().unwrap())
                .map(|payload| payload["path"].as_str().unwrap())
                .collect()
        );
        assert_eq!(
            payloads
                .iter()
                .filter(|payload| !payload["original_archive_member"].as_bool().unwrap())
                .map(|payload| payload["path"].as_str().unwrap())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "Cargo.toml",
                "PATCHES.md",
                "common/scanner.h",
                "upstream/tree-sitter-ocaml-v0.25.0.tar.gz",
            ])
        );
        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-ocaml-0.25.0/SOURCE-PROVENANCE.json"
        ))
        .unwrap();
        assert_eq!(provenance["schema"], "rootlight.ocaml-source-provenance/1");
        assert_eq!(provenance["original_inputs_retained_byte_exact"], true);
        assert_eq!(provenance["retained_native_grammar_count"], 3);
        assert_eq!(provenance["preferred_grammars_unchanged"], true);
        assert_eq!(provenance["generated_parsers_unchanged"], true);
        assert_eq!(provenance["upstream_rust_library_unchanged"], true);
        assert_eq!(provenance["upstream_rust_build_script_unchanged"], true);
        for key in ["archive", "inventory", "patch_document", "primary_license"] {
            let evidence = &provenance[key];
            let path = evidence["path"].as_str().unwrap();
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                evidence["sha256"].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                evidence["bytes"].as_u64().unwrap()
            );
        }
        for key in ["path", "bytes", "sha256"] {
            assert_eq!(provenance["archive"][key], inventory["archive"][key]);
        }
        assert_eq!(
            sources.files["common/scanner.h"],
            "7ad1a807e2a157a5aa6c16bb85697a36b4cb7d01e9313cd5296659be12d57030"
        );
        assert_eq!(
            sources.files["common/scanner.h.orig"],
            "e4934793536f53c076b24b16de2f6a48a125c632e6cce58c80c02d6d65f8dbed"
        );
    }

    #[test]
    fn xml_dtd_share_complete_sources_but_preserve_distinct_native_factories() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let directory = root.join("third_party/tree-sitter-xml-0.7.0");
        let xml = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "xml")
            .unwrap();
        let dtd = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "dtd")
            .unwrap();
        let xml_sources = xml.vendored.as_ref().unwrap();
        let dtd_sources = dtd.vendored.as_ref().unwrap();
        assert_eq!(xml_sources.files, dtd_sources.files);
        assert_eq!(xml_sources.files.len(), 148);
        assert_eq!(xml_sources.parser_path.as_deref(), Some("xml/src/parser.c"));
        assert_eq!(dtd_sources.parser_path.as_deref(), Some("dtd/src/parser.c"));
        assert_eq!(
            xml_sources.scanner_path.as_deref(),
            Some("xml/src/scanner.c")
        );
        assert_eq!(
            dtd_sources.scanner_path.as_deref(),
            Some("dtd/src/scanner.c")
        );
        for grammar in [xml, dtd] {
            assert!(grammar.crates_io_checksum.is_none());
            assert_eq!(grammar.crate_name, "tree-sitter-xml");
            assert_eq!(grammar.crate_version, "0.7.0");
            assert_eq!(grammar.abi, 14);
            assert_eq!(grammar.license, "MIT");
            assert_eq!(
                grammar.license_sha256,
                "3297073719030b7261afd78b7267931ccc03a27f7a65ca6fc644316180f71bf6"
            );
            validate_vendored_tree(&directory, grammar, grammar.vendored.as_ref().unwrap())
                .unwrap();
        }
        let binding: toml::Value = toml::from_str(include_str!(
            "../../third_party/tree-sitter-xml-0.7.0/Cargo.toml"
        ))
        .unwrap();
        assert_eq!(binding["package"]["name"].as_str(), Some("tree-sitter-xml"));
        assert_eq!(binding["package"]["publish"].as_bool(), Some(false));
        assert_eq!(binding["package"]["rust-version"].as_str(), Some("1.90"));
        assert_eq!(
            binding["package"]["build"].as_str(),
            Some("bindings/rust/build.rs")
        );
        let inventory: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-xml-0.7.0/SOURCE-INVENTORY.json"
        ))
        .unwrap();
        assert_eq!(inventory["schema"], "rootlight.xml-source-inventory/1");
        assert_eq!(inventory["original_source_frame_file_count"], 117);
        assert_eq!(inventory["original_source_frame_bytes"], 1_751_022);
        assert_eq!(
            inventory["original_source_frame_directories"]
                .as_array()
                .unwrap()
                .len(),
            44
        );
        let mut paths = BTreeSet::new();
        for payload in inventory["payload_files"].as_array().unwrap() {
            let path = payload["path"].as_str().unwrap();
            assert!(is_source_relative_path(path));
            assert!(paths.insert(path));
            assert_eq!(
                xml_sources.files.get(path).map(String::as_str),
                payload["sha256"].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                payload["bytes"].as_u64().unwrap()
            );
        }
        assert_eq!(paths.len(), 146);
        paths.extend(["SOURCE-INVENTORY.json", "SOURCE-PROVENANCE.json"]);
        assert_eq!(
            paths,
            xml_sources.files.keys().map(String::as_str).collect()
        );
        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-xml-0.7.0/SOURCE-PROVENANCE.json"
        ))
        .unwrap();
        assert_eq!(provenance["schema"], "rootlight.xml-source-provenance/1");
        assert_eq!(
            provenance["upstream"]["revision"],
            "4b64dd3a03ec002258d6268d712fd93716d6ab57"
        );
        assert_eq!(
            provenance["preferred_source"]["xml_scanner_stateless"],
            true
        );
        assert_eq!(
            provenance["preferred_source"]["dtd_grammar_and_scanner_literal_upstream"],
            true
        );
        assert_eq!(
            provenance["native_written_syntax_evidence"]["fixed_sources"],
            39
        );
        assert_eq!(
            provenance["native_written_syntax_evidence"]["raw_snapshots"],
            488
        );
        assert_eq!(
            provenance["native_written_syntax_evidence"]["nontrivia_parent_range_gap_bytes"],
            617
        );
        // These historical source-only flags must not be rewritten by integration.
        for key in [
            "source_publication_verified",
            "relocated_package_qualification_verified",
            "product_integration_verified",
            "installed_mcp_verified",
            "offline_empty_cache_network_denial_verified",
            "distribution_notices_integrated",
        ] {
            assert_eq!(provenance[key], false, "{key}");
        }
        assert_eq!(
            xml_sources.files["notices/tree-sitter-MIT-LICENSE"],
            "c5cfb43042b6b72045f4ba997834d0a7786d2793d91680868b5815b39f14fc78"
        );
    }

    #[test]
    fn assembly_retains_originals_and_authenticates_scoped_native_tables() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "assembly")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        let directory = root.join("third_party/tree-sitter-asm-0.24.0");
        assert!(grammar.crates_io_checksum.is_none());
        assert_eq!(grammar.abi, 14);
        assert_eq!(grammar.scanner_sha256, "none");
        assert_eq!(sources.files.len(), 89);
        assert_eq!(sources.parser_path.as_deref(), Some("src/parser.c"));
        assert_eq!(sources.license_path.as_deref(), Some("notices/LICENSE"));
        assert_eq!(
            grammar.license_sha256,
            "0e9f43573925c2e4b49f328a45c4df22c882191d77d876b0d18ecd60bd21e17c"
        );
        let inventory: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-asm-0.24.0/SOURCE-INVENTORY.json"
        ))
        .unwrap();
        assert_eq!(inventory["schema"], "rootlight.assembly-source-inventory/1");
        assert_eq!(inventory["original_regular_file_count"], 66);
        assert_eq!(inventory["original_regular_file_bytes"], 399_975);
        assert_eq!(inventory["original_directory_record_count"], 20);
        let mut payload_paths = BTreeSet::new();
        for payload in inventory["payload_files"].as_array().unwrap() {
            let path = payload["path"].as_str().unwrap();
            assert!(is_source_relative_path(path));
            assert!(payload_paths.insert(path));
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                payload["sha256"].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                payload["bytes"]
            );
        }
        assert_eq!(payload_paths.len(), 87);
        payload_paths.extend(["SOURCE-INVENTORY.json", "SOURCE-PROVENANCE.json"]);
        assert_eq!(
            payload_paths,
            sources.files.keys().map(String::as_str).collect()
        );
        let mut original_paths = BTreeSet::new();
        let mut original_bytes = 0u64;
        let mut original_directories = 0usize;
        for archive in inventory["sources"].as_array().unwrap() {
            let retained_tree = archive["retained_tree"].as_str().unwrap();
            let archive_path = archive["archive"].as_str().unwrap();
            assert_eq!(
                sources.files.get(archive_path).map(String::as_str),
                archive["sha256"].as_str()
            );
            for member in archive["inventory"]["members"].as_array().unwrap() {
                let member_path = member["path"].as_str().unwrap();
                let path = if member_path.is_empty() {
                    assert_eq!(member["directory"], true);
                    retained_tree.to_owned()
                } else {
                    format!("{retained_tree}/{member_path}")
                };
                assert!(is_source_relative_path(&path));
                if member["directory"].as_bool().unwrap() {
                    assert!(directory.join(&path).is_dir());
                    original_directories += 1;
                } else {
                    assert!(original_paths.insert(path.clone()));
                    assert_eq!(
                        sources.files.get(&path).map(String::as_str),
                        member["sha256"].as_str()
                    );
                    original_bytes += member["bytes"].as_u64().unwrap();
                }
            }
        }
        assert_eq!(original_paths.len(), 66);
        assert_eq!(original_bytes, 399_975);
        assert_eq!(original_directories, 20);
        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-asm-0.24.0/SOURCE-PROVENANCE.json"
        ))
        .unwrap();
        assert_eq!(provenance["package"], grammar.crate_name);
        assert_eq!(provenance["version"], grammar.crate_version);
        assert_eq!(provenance["authored_revision"], grammar.commit);
        assert_eq!(provenance["license"], "MIT");
        let generation = &provenance["local_generation"];
        assert_eq!(generation["abi"], 14);
        assert_eq!(generation["state_count"], 179);
        assert_eq!(generation["symbol_count"], 84);
        assert_eq!(generation["external_token_count"], 0);
        let outputs = generation["outputs"].as_object().unwrap();
        assert_eq!(outputs.len(), 6);
        for (path, output) in outputs {
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                output["sha256"].as_str()
            );
        }
        for claim in [
            "production_integration_verified",
            "whole_language_coverage_verified",
            "installed_mcp_verified",
            "build_and_packaging_acceptance_verified",
            "empty_dependency_cache_verified",
            "os_network_denial_verified",
        ] {
            assert_eq!(provenance[claim], false, "{claim}");
        }
        validate_vendored_tree(&directory, grammar, sources)
            .expect("all original Assembly inputs, the primary notice and native tables agree");
    }

    #[test]
    fn glsl_retains_originals_and_authenticates_scoped_native_tables() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "glsl")
            .expect("GLSL source evidence is present");
        let sources = grammar.vendored.as_ref().unwrap();
        let directory = root.join("third_party/tree-sitter-glsl-0.2.0");
        assert!(grammar.crates_io_checksum.is_none());
        assert_eq!(grammar.abi, 14);
        assert_eq!(grammar.scanner_sha256, "none");
        assert_eq!(sources.files.len(), 161);
        assert_eq!(sources.parser_path.as_deref(), Some("src/parser.c"));
        assert_eq!(sources.license_path.as_deref(), Some("LICENSE"));
        assert_eq!(
            grammar.license_sha256,
            "f44d7107a33801260dc82624c91a71ebf5918b4038a0fb0e3ef60e2dd9ba1999"
        );
        assert_eq!(
            sources.files["notices/tree-sitter-c-LICENSE"],
            "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1"
        );
        let binding: toml::Value = toml::from_str(include_str!(
            "../../third_party/tree-sitter-glsl-0.2.0/Cargo.toml"
        ))
        .unwrap();
        assert_eq!(
            binding["package"]["name"].as_str(),
            Some(grammar.crate_name.as_str())
        );
        assert_eq!(
            binding["package"]["version"].as_str(),
            Some(grammar.crate_version.as_str())
        );
        assert_eq!(binding["package"]["publish"].as_bool(), Some(false));
        assert_eq!(binding["package"]["rust-version"].as_str(), Some("1.90"));
        assert_eq!(
            binding["package"]["build"].as_str(),
            Some("bindings/rust/build.rs")
        );
        let inventory: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-glsl-0.2.0/SOURCE-INVENTORY.json"
        ))
        .unwrap();
        assert_eq!(inventory["schema"], "rootlight.glsl-source-inventory/1");
        assert_eq!(inventory["original_regular_file_count"], 134);
        assert_eq!(inventory["original_regular_file_bytes"], 16_566_104);
        assert_eq!(inventory["original_directory_record_count"], 44);
        assert_eq!(
            inventory["original_modes_and_executable_intent_retained"],
            true
        );
        let mut payload_paths = BTreeSet::new();
        for payload in inventory["payload_files"].as_array().unwrap() {
            let path = payload["path"].as_str().unwrap();
            assert!(is_source_relative_path(path));
            assert!(payload_paths.insert(path));
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                payload["sha256"].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                payload["bytes"]
            );
        }
        assert_eq!(payload_paths.len(), 159);
        payload_paths.extend(["SOURCE-INVENTORY.json", "SOURCE-PROVENANCE.json"]);
        assert_eq!(
            payload_paths,
            sources.files.keys().map(String::as_str).collect()
        );
        let mut original_paths = BTreeSet::new();
        let mut original_bytes = 0u64;
        let mut original_directories = 0usize;
        assert_eq!(inventory["sources"].as_array().unwrap().len(), 3);
        for archive in inventory["sources"].as_array().unwrap() {
            let retained_tree = archive["retained_tree"].as_str().unwrap();
            let archive_path = archive["archive"].as_str().unwrap();
            assert_eq!(
                sources.files.get(archive_path).map(String::as_str),
                archive["sha256"].as_str()
            );
            let members = archive["inventory"]["members"].as_array().unwrap();
            for member in members {
                let member_path = member["path"].as_str().unwrap();
                let path = if member_path.is_empty() {
                    assert_eq!(member["directory"], true);
                    retained_tree.to_owned()
                } else {
                    format!("{retained_tree}/{member_path}")
                };
                assert!(is_source_relative_path(&path));
                if member["directory"].as_bool().unwrap() {
                    assert!(archived_directory_materialized(
                        &directory.join(&path),
                        member_path,
                        members
                    ));
                    original_directories += 1;
                } else {
                    assert!(original_paths.insert(path.clone()));
                    assert_eq!(
                        sources.files.get(&path).map(String::as_str),
                        member["sha256"].as_str()
                    );
                    original_bytes += member["bytes"].as_u64().unwrap();
                }
            }
        }
        assert_eq!(original_paths.len(), 134);
        assert_eq!(original_bytes, 16_566_104);
        assert_eq!(original_directories, 44);
        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-glsl-0.2.0/SOURCE-PROVENANCE.json"
        ))
        .unwrap();
        assert_eq!(provenance["schema"], "rootlight.glsl-source-provenance/1");
        assert_eq!(provenance["package"], grammar.crate_name);
        assert_eq!(provenance["version"], grammar.crate_version);
        assert_eq!(provenance["authored_revision"], grammar.commit);
        assert_eq!(provenance["license"], "MIT");
        assert_eq!(
            provenance["notice_paths"],
            json!(["LICENSE", "notices/tree-sitter-c-LICENSE"])
        );
        assert!(provenance["preferred_generation_parent"]["registry_publication"].is_null());
        assert_eq!(
            provenance["inventory"]["sha256"],
            sources.files["SOURCE-INVENTORY.json"]
        );
        assert_eq!(
            provenance["inventory"]["bytes"],
            fs::metadata(directory.join("SOURCE-INVENTORY.json"))
                .unwrap()
                .len()
        );
        let generation = &provenance["local_generation"];
        assert_eq!(generation["abi"], 14);
        assert_eq!(generation["state_count"], 2_574);
        assert_eq!(generation["symbol_count"], 409);
        assert_eq!(generation["external_token_count"], 0);
        assert_eq!(generation["producer_count"], 2);
        assert_eq!(generation["six_output_pairs_literal_equal"], true);
        let mut generated_paths = BTreeSet::new();
        for output in generation["exact_generated_outputs"].as_array().unwrap() {
            let path = output["path"].as_str().unwrap();
            assert!(generated_paths.insert(path));
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                output["sha256"].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                output["bytes"]
            );
        }
        assert_eq!(
            generated_paths,
            BTreeSet::from([
                "src/grammar.json",
                "src/node-types.json",
                "src/parser.c",
                "src/tree_sitter/alloc.h",
                "src/tree_sitter/array.h",
                "src/tree_sitter/parser.h",
            ])
        );
        // The published record authenticates the isolated proof, not a later
        // relocated run or product binding qualification.
        let native = &provenance["isolated_native_evidence"];
        assert_eq!(native["relocated_wrapper_executed"], false);
        assert_eq!(native["unchanged_groups_per_profile"], 6);
        assert_eq!(native["full_source_observations_per_profile"], 8);
        for claim in [
            "production_integration_verified",
            "whole_language_coverage_verified",
            "installed_mcp_verified",
            "build_and_packaging_acceptance_verified",
            "complete_product_source_closure_proven",
            "empty_dependency_cache_verified",
            "os_network_denial_verified",
            "source_download_or_generation_at_build_time",
            "archived_upstream_lifecycle_activated",
            "precision_semantic_propagation_verified",
            "discard_dedicated_statement_or_resolved_reference_verified",
        ] {
            assert_eq!(provenance[claim], false, "{claim}");
        }
        validate_vendored_tree(&directory, grammar, sources)
            .expect("all original GLSL and C inputs, notices and native tables agree");
    }

    #[test]
    fn wgsl_retains_complete_originals_and_exact_preferred_native_and_notice_inputs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "wgsl")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        let directory = root.join("third_party/tree-sitter-wgsl-0.0.9");
        assert!(grammar.crates_io_checksum.is_none());
        assert_eq!(grammar.abi, 14);
        assert_eq!(
            grammar.vendored_source_sha256.as_deref(),
            Some("4a6bd6793fc477b90b2ded2d5c7f449485e1c19473fad36b90fb8777e0b60b3e")
        );
        assert_eq!(sources.files.len(), 38);
        assert_eq!(sources.parser_path.as_deref(), Some("src/parser.c"));
        assert_eq!(sources.scanner_path.as_deref(), Some("src/scanner.c"));
        assert_eq!(sources.license_path.as_deref(), Some("LICENSE.md"));
        let inventory: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-wgsl-0.0.9/SOURCE-INVENTORY.json"
        ))
        .unwrap();
        assert_eq!(inventory["schema"], "rootlight.wgsl-source-inventory/1");
        assert_eq!(inventory["original_regular_file_count"], 13);
        assert_eq!(inventory["original_regular_file_bytes"], 207_378);
        assert_eq!(inventory["original_directory_record_count"], 3);
        assert_eq!(
            inventory["original_modes_and_executable_intent_retained"],
            true
        );
        assert_eq!(inventory["archive_modes_applied"], false);
        let mut payloads = BTreeSet::new();
        for member in inventory["payload_files"].as_array().unwrap() {
            let path = member["path"].as_str().unwrap();
            assert!(payloads.insert(path));
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                member["sha256"].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                member["bytes"]
            );
        }
        assert_eq!(payloads.len(), 36);
        payloads.extend(["SOURCE-INVENTORY.json", "SOURCE-PROVENANCE.json"]);
        assert_eq!(payloads, sources.files.keys().map(String::as_str).collect());
        let archives = inventory["sources"].as_array().unwrap();
        assert_eq!(archives.len(), 1);
        let archived = &archives[0];
        assert_eq!(
            sources
                .files
                .get(archived["archive"].as_str().unwrap())
                .map(String::as_str),
            archived["sha256"].as_str()
        );
        let members = archived["inventory"]["members"].as_array().unwrap();
        let retained_tree = archived["retained_tree"].as_str().unwrap();
        let mut originals = 0;
        let mut directories = 0;
        let mut original_bytes = 0u64;
        for member in members {
            let path = member["path"].as_str().unwrap();
            let retained = if path.is_empty() {
                retained_tree.to_owned()
            } else {
                format!("{retained_tree}/{path}")
            };
            let is_directory = member["directory"].as_bool().unwrap();
            assert_eq!(
                member["executable_intent"].as_bool(),
                Some(!is_directory && member["raw_mode"].as_u64().unwrap() & 0o111 != 0)
            );
            if is_directory {
                assert!(archived_directory_materialized(
                    &directory.join(&retained),
                    path,
                    members
                ));
                directories += 1;
            } else {
                assert_eq!(
                    sources.files.get(&retained).map(String::as_str),
                    member["sha256"].as_str()
                );
                originals += 1;
                original_bytes += member["bytes"].as_u64().unwrap();
            }
        }
        assert_eq!((originals, directories, original_bytes), (13, 3, 207_378));
        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-wgsl-0.0.9/SOURCE-PROVENANCE.json"
        ))
        .unwrap();
        assert_eq!(provenance["authored_revision"], grammar.commit);
        assert_eq!(provenance["mirror_original_wasm"]["retained_inert"], true);
        assert_eq!(provenance["mirror_original_wasm"]["executed"], false);
        assert_eq!(provenance["local_generation"]["producer_count"], 1);
        assert_eq!(
            provenance["local_generation"]["twin_producer_reproducibility_verified"],
            false
        );
        assert_eq!(provenance["original_scanner_inert"], true);
        assert_eq!(
            provenance["preferred_scanner"]["sha256"],
            grammar.scanner_sha256
        );
        for claim in [
            "installed_mcp_verified",
            "whole_language_coverage_verified",
            "complete_product_source_closure_proven",
            "empty_dependency_cache_verified",
            "os_network_denial_verified",
            "source_download_or_generation_at_build_time",
            "archived_upstream_lifecycle_activated",
            "license_policy_or_legal_acceptance_verified",
        ] {
            assert_eq!(provenance[claim], false, "{claim}");
        }
        validate_vendored_tree(&directory, grammar, sources).unwrap();
        for (path, digest) in [
            (
                "third_party/tree-sitter-wgsl-0.0.9/LICENSE.md",
                "18d40c8ea3118033e6b4725efa522ac00bceb8ae429a1a0fa4356152bb07dc56",
            ),
            (
                "third_party/tree-sitter-wgsl-0.0.9/notices/gpuweb-bsd-463e2f34-LICENSE.txt",
                "83d286b68d185bc8d3299b655e317ed888a9b7af1fe3a518b0237ab03896a0d7",
            ),
            (
                "third_party/tree-sitter-wgsl-0.0.9/notices/w3c-software-license-2023.html",
                "5497437f8e227f2a64def93edfccc6a85088a5c8e1924cacf419090e57c7c058",
            ),
            (
                "third_party/tree-sitter-wgsl-0.0.9/notices/tree-sitter-MIT-LICENSE",
                "c5cfb43042b6b72045f4ba997834d0a7786d2793d91680868b5815b39f14fc78",
            ),
            (
                "crates/rootlight-unicode/data/XID-16.0.0-LICENSE.txt",
                "f7db81051789b729fea528a63ec4c938fdcb93d9d61d97dc8cc2e9df6d47f2a1",
            ),
        ] {
            validate_local_license(&root, path, digest).unwrap();
        }
    }

    #[test]
    fn hlsl_retains_originals_and_authenticates_scoped_native_tables() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "hlsl")
            .expect("HLSL source evidence is present");
        let sources = grammar.vendored.as_ref().unwrap();
        let directory = root.join("third_party/tree-sitter-hlsl-0.2.0");
        assert!(grammar.crates_io_checksum.is_none());
        assert_eq!(grammar.abi, 14);
        assert_eq!(
            grammar.scanner_sha256,
            "599f387a256272fe8da1a76d9c1402679882146d9a18a641cb8252a6fae64b37"
        );
        assert_eq!(sources.files.len(), 243);
        assert_eq!(sources.parser_path.as_deref(), Some("src/parser.c"));
        assert_eq!(sources.scanner_path.as_deref(), Some("src/scanner.c"));
        assert_eq!(sources.license_path.as_deref(), Some("LICENSE"));
        assert_eq!(
            grammar.license_sha256,
            "dec735ccc031754dd05e6556dd9e959915eb367cf01cf009c77a1a38786932b1"
        );
        assert_eq!(
            sources.files["notices/tree-sitter-c-LICENSE"],
            "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1"
        );
        assert_eq!(
            sources.files["notices/tree-sitter-cpp-LICENSE"],
            "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1"
        );
        let binding: toml::Value = toml::from_str(include_str!(
            "../../third_party/tree-sitter-hlsl-0.2.0/Cargo.toml"
        ))
        .unwrap();
        assert_eq!(
            binding["package"]["name"].as_str(),
            Some(grammar.crate_name.as_str())
        );
        assert_eq!(
            binding["package"]["version"].as_str(),
            Some(grammar.crate_version.as_str())
        );
        assert_eq!(binding["package"]["publish"].as_bool(), Some(false));
        assert_eq!(binding["package"]["rust-version"].as_str(), Some("1.90"));
        assert_eq!(
            binding["package"]["build"].as_str(),
            Some("bindings/rust/build.rs")
        );
        let inventory: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-hlsl-0.2.0/SOURCE-INVENTORY.json"
        ))
        .unwrap();
        assert_eq!(inventory["schema"], "rootlight.hlsl-source-inventory/1");
        assert_eq!(inventory["original_regular_file_count"], 212);
        assert_eq!(inventory["original_regular_file_bytes"], 66_498_950);
        assert_eq!(inventory["original_directory_record_count"], 65);
        assert_eq!(
            inventory["original_modes_and_executable_intent_retained"],
            true
        );
        assert_eq!(inventory["archive_modes_applied"], false);
        let mut payload_paths = BTreeSet::new();
        for payload in inventory["payload_files"].as_array().unwrap() {
            let path = payload["path"].as_str().unwrap();
            assert!(is_source_relative_path(path));
            assert!(payload_paths.insert(path));
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                payload["sha256"].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                payload["bytes"]
            );
        }
        assert_eq!(payload_paths.len(), 241);
        payload_paths.extend(["SOURCE-INVENTORY.json", "SOURCE-PROVENANCE.json"]);
        assert_eq!(
            payload_paths,
            sources.files.keys().map(String::as_str).collect()
        );
        let mut original_paths = BTreeSet::new();
        let mut original_bytes = 0u64;
        let mut original_directories = 0usize;
        assert_eq!(inventory["sources"].as_array().unwrap().len(), 4);
        for archive in inventory["sources"].as_array().unwrap() {
            let retained_tree = archive["retained_tree"].as_str().unwrap();
            let archive_path = archive["archive"].as_str().unwrap();
            assert_eq!(
                sources.files.get(archive_path).map(String::as_str),
                archive["sha256"].as_str()
            );
            let members = archive["inventory"]["members"].as_array().unwrap();
            for member in members {
                let member_path = member["path"].as_str().unwrap();
                let path = if member_path.is_empty() {
                    assert_eq!(member["directory"], true);
                    retained_tree.to_owned()
                } else {
                    format!("{retained_tree}/{member_path}")
                };
                assert!(is_source_relative_path(&path));
                let is_directory = member["directory"].as_bool().unwrap();
                assert_eq!(
                    member["executable_intent"].as_bool(),
                    Some(!is_directory && member["raw_mode"].as_u64().unwrap() & 0o111 != 0)
                );
                if is_directory {
                    assert!(archived_directory_materialized(
                        &directory.join(&path),
                        member_path,
                        members
                    ));
                    original_directories += 1;
                } else {
                    assert!(original_paths.insert(path.clone()));
                    assert_eq!(
                        sources.files.get(&path).map(String::as_str),
                        member["sha256"].as_str()
                    );
                    original_bytes += member["bytes"].as_u64().unwrap();
                }
            }
        }
        assert_eq!(original_paths.len(), 212);
        assert_eq!(original_bytes, 66_498_950);
        assert_eq!(original_directories, 65);
        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-hlsl-0.2.0/SOURCE-PROVENANCE.json"
        ))
        .unwrap();
        assert_eq!(provenance["schema"], "rootlight.hlsl-source-provenance/1");
        assert_eq!(provenance["package"], grammar.crate_name);
        assert_eq!(provenance["version"], grammar.crate_version);
        assert_eq!(provenance["authored_revision"], grammar.commit);
        assert_eq!(provenance["license"], "MIT");
        assert_eq!(
            provenance["notice_paths"],
            json!([
                "LICENSE",
                "notices/tree-sitter-cpp-LICENSE",
                "notices/tree-sitter-c-LICENSE"
            ])
        );
        let parents = &provenance["preferred_generation_parent_closure"];
        assert_eq!(
            parents["cpp"]["revision"],
            "e5cea0ec884c5c3d2d1e41a741a66ce13da4d945"
        );
        assert_eq!(
            parents["c"]["revision"],
            "2a265d69a4caf57108a73ad2ed1e6922dd2f998c"
        );
        assert_eq!(parents["hlsl_import"], "tree-sitter-cpp/grammar");
        assert_eq!(parents["cpp_import"], "tree-sitter-c/grammar");
        assert_eq!(parents["registry_substitution"], false);
        assert_eq!(parents["build_time_generation"], false);
        let archive_pins = provenance["archive_identity_pins"].as_array().unwrap();
        assert_eq!(archive_pins.len(), 4);
        for pin in archive_pins {
            let path = pin["archive"].as_str().unwrap();
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                pin["sha256"].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                pin["bytes"]
            );
            let archived = inventory["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|source| source["archive"] == pin["archive"])
                .expect("each provenance archive has an exact source inventory");
            assert_eq!(archived["sha256"], pin["sha256"]);
            assert_eq!(archived["bytes"], pin["bytes"]);
        }
        assert_eq!(
            provenance["inventory"]["sha256"],
            sources.files["SOURCE-INVENTORY.json"]
        );
        assert_eq!(
            provenance["inventory"]["bytes"],
            fs::metadata(directory.join("SOURCE-INVENTORY.json"))
                .unwrap()
                .len()
        );
        let generation = &provenance["local_generation"];
        assert_eq!(generation["abi"], 14);
        assert_eq!(generation["state_count"], 8_842);
        assert_eq!(generation["symbol_count"], 582);
        assert_eq!(generation["external_token_count"], 2);
        assert_eq!(generation["producer_count"], 2);
        assert_eq!(generation["six_output_pairs_literal_equal"], true);
        let mut generated_paths = BTreeSet::new();
        for output in generation["exact_generated_outputs"].as_array().unwrap() {
            let path = output["path"].as_str().unwrap();
            assert!(generated_paths.insert(path));
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                output["sha256"].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                output["bytes"]
            );
        }
        assert_eq!(
            generated_paths,
            BTreeSet::from([
                "src/grammar.json",
                "src/node-types.json",
                "src/parser.c",
                "src/tree_sitter/alloc.h",
                "src/tree_sitter/array.h",
                "src/tree_sitter/parser.h",
            ])
        );
        // The published record authenticates the isolated proof, not a later
        // relocated run or product binding qualification.
        let native = &provenance["isolated_native_evidence"];
        assert_eq!(native["relocated_wrapper_executed"], false);
        assert_eq!(native["groups"], 7);
        assert_eq!(native["observations"], 9);
        assert_eq!(native["actual_test_exit_codes"], json!([0, 0]));
        assert_eq!(native["scoped_clippy_exit_code"], 0);
        assert_eq!(native["source_bytes"], 1_901);
        assert_eq!(native["leaf_bytes"], 1_729);
        assert_eq!(native["whitespace_gap_bytes"], 172);
        assert_eq!(native["trusted_original_producer_only"], true);
        assert_eq!(native["malformed_original_state_executed"], false);
        let callbacks = &provenance["preferred_callback_evidence"];
        assert_eq!(callbacks["scanner_pin"]["sha256"], grammar.scanner_sha256);
        assert_eq!(callbacks["scanner_pin"]["bytes"], 5_403);
        assert_eq!(callbacks["accepted_lengths"], 16);
        assert_eq!(callbacks["rejected_lengths"], 5);
        assert_eq!(callbacks["null_payload"], true);
        assert_eq!(callbacks["fixture_private_allocation_failure"], true);
        assert_eq!(callbacks["recorded_by_root_actual_compiler_exit_code"], 0);
        assert_eq!(callbacks["recorded_by_root_actual_callback_exit_code"], 0);
        assert_eq!(callbacks["inherited_compiler_warning_count"], 2);
        assert_eq!(callbacks["warning_clean_claim"], false);
        assert_eq!(provenance["unsafe_original_scanner_inert"], true);
        assert_ne!(
            sources.files["src/scanner.c"],
            sources.files["upstream/hlsl-published/src/scanner.c"]
        );
        for claim in [
            "production_integration_verified",
            "whole_language_coverage_verified",
            "installed_mcp_verified",
            "build_and_packaging_acceptance_verified",
            "complete_product_source_closure_proven",
            "empty_dependency_cache_verified",
            "os_network_denial_verified",
            "source_download_or_generation_at_build_time",
            "archived_upstream_lifecycle_activated",
            "whole_language_syntax_validity_verified",
            "relocated_wrapper_executed",
            "physical_cbuffer_name_is_declaration_scope_or_resolved_identity",
            "runtime_register_or_profile_assignment_verified",
            "inherited_permissive_local_buffer_branches_are_hlsl_validity_proof",
        ] {
            assert_eq!(provenance[claim], false, "{claim}");
        }
        validate_vendored_tree(&directory, grammar, sources).expect(
            "all original HLSL, C++ and C inputs, notices and preferred native tables agree",
        );
    }

    #[test]
    fn local_multi_profile_records_require_one_reviewed_source_tree() {
        for (language, package, review_path) in [
            ("cobol-fixed", "tree-sitter-cobol", "PATCHES.md"),
            ("cobol-free", "tree-sitter-cobol", "PATCHES.md"),
            ("fsharp", "tree-sitter-fsharp", "ROOTLIGHT-PATCH.md"),
            (
                "fsharp-signature",
                "tree-sitter-fsharp",
                "ROOTLIGHT-PATCH.md",
            ),
        ] {
            let mut manifest: GrammarLock =
                toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
            let grammar = manifest
                .grammars
                .iter_mut()
                .find(|grammar| grammar.language == language)
                .unwrap();
            let files = &mut grammar.vendored.as_mut().unwrap().files;
            files.insert(review_path.into(), sha256_hex(b"different source review"));
            grammar.vendored_source_sha256 = Some(vendored_source_digest(files).unwrap());
            validate_grammar(grammar)
                .expect("self-consistent manifest is not the reviewed source pin");
            assert!(matches!(validate_manifest(&manifest),
                Err(GrammarLockError::PackageEvidence { package: observed }) if observed == package));
        }
    }

    #[test]
    fn fsharp_profiles_authenticate_shared_sources_and_distinct_parser_paths() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let implementation = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "fsharp")
            .unwrap();
        let signature = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "fsharp-signature")
            .unwrap();
        let implementation_sources = implementation.vendored.as_ref().unwrap();
        let signature_sources = signature.vendored.as_ref().unwrap();
        assert_eq!(implementation_sources.files, signature_sources.files);
        assert_eq!(implementation_sources.files.len(), 33);
        assert_eq!(
            implementation_sources.parser_path.as_deref(),
            Some("fsharp/src/parser.c")
        );
        assert_eq!(
            signature_sources.parser_path.as_deref(),
            Some("fsharp_signature/src/parser.c")
        );
        assert_ne!(implementation.parser_sha256, signature.parser_sha256);
        for grammar in [implementation, signature] {
            let sources = grammar.vendored.as_ref().unwrap();
            assert_eq!(sources.scanner_path.as_deref(), Some("common/scanner.h"));
            assert_eq!(
                sources.files.get("common/scanner.h"),
                Some(&grammar.scanner_sha256)
            );
            for path in [
                "common/scanner.h.orig",
                "fsharp/grammar.js.orig",
                "fsharp_signature/grammar.js.orig",
                "fsharp/src/scanner.c",
                "fsharp_signature/src/scanner.c",
                "SOURCE-PROVENANCE.json",
                "ROOTLIGHT-PATCH.md",
                "LICENSE",
            ] {
                assert!(
                    sources.files.contains_key(path),
                    "missing retained input: {path}"
                );
            }
            validate_vendored_tree(
                &root.join("third_party/tree-sitter-fsharp-0.3.12"),
                grammar,
                sources,
            )
            .expect("both profiles authenticate the complete reviewed source tree");
        }
    }

    #[test]
    fn fortran_retains_original_inputs_and_declares_scanner_patch() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "fortran")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        for (path, expected) in [
            (
                "src/scanner.c.orig",
                "84487995be2bdfeba996baa7c983affb8dd33d0c38faeaadaab7a4b1fda9aa7c",
            ),
            (
                "upstream/tree-sitter-fortran-0.6.0.crate",
                "ea5fea41363a9b59666001dc26c4b356d4e164da27c6ce31145e00aba6bf9b16",
            ),
            (
                "upstream/tree-sitter-fortran-48593829df8a929bfe58528eedcf405171921005.tar.gz",
                "e7f87ad365f254b4048b7a3655407ea669382ff2b4ae7cfd3f5cbdabab16dacd",
            ),
        ] {
            assert_eq!(sources.files.get(path).map(String::as_str), Some(expected));
        }
        for path in [
            "grammar.js",
            "src/node-types.json",
            "LICENSE",
            "PATCHES.md",
            "SOURCE-PROVENANCE.json",
        ] {
            assert!(
                sources.files.contains_key(path),
                "missing retained input: {path}"
            );
        }
        assert_ne!(
            sources.files["src/scanner.c"],
            sources.files["src/scanner.c.orig"]
        );
        validate_vendored_tree(
            &root.join("third_party/tree-sitter-fortran-0.6.0"),
            grammar,
            sources,
        )
        .expect("the complete authored tree and local safety patch are authenticated");
    }

    #[test]
    fn sql_retains_published_inputs_and_authenticates_local_tables() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "sql")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        assert_eq!(sources.files.len(), 25);
        for (path, expected) in [
            (
                "grammar.js.orig",
                "6b1f0554a355d8fe3b341ef25c7236cf4b433f85b632e39ade62931b78de16bc",
            ),
            (
                "src/grammar.json.orig",
                "d0ce0452876e14e73b533dd2a3010fbded021a55d153d6e6842cd0127efa971c",
            ),
            (
                "src/node-types.json.orig",
                "227cb2197775686cc0b0064aa08e709f7f9575b724b2339371b10fb209e37d73",
            ),
            (
                "src/parser.c.orig",
                "852e088fb8470952cdb2a1b78c1c58626c7d91562b26baa4672d51f9754bf580",
            ),
            (
                "src/tree_sitter/alloc.h.orig",
                "b29c1c9fb7cc82f58c84b376df1297d6e2737a1d655fd356db0859e3c29c2fea",
            ),
            (
                "src/tree_sitter/array.h.orig",
                "5bdf6ed1a78e3409fd443e085ca967a64c188a5d082aaf7f819bccd53a471c94",
            ),
            (
                "src/tree_sitter/parser.h.orig",
                "a1f6ef161fbaf48a0e10fca90ef5290a062462b307b3898aa562993853b9f80a",
            ),
        ] {
            assert_eq!(sources.files.get(path).map(String::as_str), Some(expected));
        }
        assert_ne!(
            sources.files["grammar.js"],
            sources.files["grammar.js.orig"]
        );
        assert_ne!(
            sources.files["src/parser.c"],
            sources.files["src/parser.c.orig"]
        );
        assert_eq!(grammar.parser_sha256, sources.files["src/parser.c"]);
        validate_vendored_tree(
            &root.join("third_party/tree-sitter-sequel-0.3.11"),
            grammar,
            sources,
        )
        .expect("the retained published inputs and active SQL native tables are authenticated");
    }

    #[test]
    fn plsql_retains_original_inputs_and_authenticates_local_generation() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "plsql")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        assert!(grammar.crates_io_checksum.is_none());
        assert_eq!(grammar.abi, 14);
        assert_eq!(grammar.scanner_sha256, "none");
        assert_eq!(sources.files.len(), 39);
        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-plsql-0.0.1/SOURCE-PROVENANCE.json"
        ))
        .unwrap();
        let originals = provenance["upstream_original_files"].as_array().unwrap();
        assert_eq!(originals.len(), 27);
        for original in originals {
            let retained = original["retained_path"].as_str().unwrap();
            assert_eq!(
                sources.files.get(retained).map(String::as_str),
                original["sha256"].as_str(),
                "retained original source differs: {retained}"
            );
        }
        let changes = provenance["source_modifications"].as_array().unwrap();
        assert_eq!(changes.len(), 8);
        for change in changes {
            let path = change["path"].as_str().unwrap();
            let original = change["original_path"].as_str().unwrap();
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                change["local_sha256"].as_str()
            );
            assert_eq!(
                sources.files.get(original).map(String::as_str),
                change["original_sha256"].as_str()
            );
            assert_ne!(sources.files[path], sources.files[original]);
            assert_eq!(
                change["patch_document_sha256"].as_str(),
                sources.files.get("PATCHES.md").map(String::as_str)
            );
        }
        assert_eq!(
            provenance["original_generated_parser_unchanged"],
            serde_json::Value::Bool(false)
        );
        assert_eq!(
            provenance["preferred_grammar_unchanged"],
            serde_json::Value::Bool(false)
        );
        assert_eq!(provenance["generated_parser_language_abi"], 14);
        assert_eq!(provenance["generated_parser_state_count"], 10623);
        assert_eq!(provenance["generated_parser_external_token_count"], 0);
        let generation = &provenance["local_generation"];
        assert_eq!(generation["version"], "0.24.4");
        assert_eq!(generation["javascript_runtime"], "Node.js 24.11.1");
        assert_eq!(
            generation["preferred_input_sha256"].as_str(),
            sources.files.get("grammar.js").map(String::as_str)
        );
        assert_eq!(generation["independent_regeneration_count"], 2);
        assert_eq!(
            generation["independent_regeneration_exit_codes"],
            serde_json::json!([0, 0])
        );
        assert_eq!(
            generation["independent_regeneration_stderr_bytes"],
            serde_json::json!([0, 0])
        );
        assert_eq!(
            generation["independent_regenerations_byte_identical"],
            serde_json::Value::Bool(true)
        );
        assert_eq!(generation["unchanged_existing_grammar_rule_count"], 747);
        assert_eq!(
            generation["modified_existing_grammar_rules"]
                .as_array()
                .unwrap()
                .len(),
            9
        );
        assert_eq!(generation["new_grammar_rule_count"], 29);
        assert_eq!(generation["removed_existing_grammar_rule_count"], 0);
        assert_eq!(generation["grammar_conflicts_and_extras_unchanged"], false);
        assert_eq!(generation["original_grammar_conflicts_retained"], true);
        assert_eq!(generation["grammar_extras_unchanged"], true);
        assert_eq!(
            generation["added_grammar_conflicts"],
            serde_json::json!([["result_cache_clause"]])
        );
        let outputs = generation["generated_output_sha256"].as_object().unwrap();
        assert_eq!(outputs.len(), 6);
        for (path, checksum) in outputs {
            assert_eq!(
                checksum.as_str(),
                sources.files.get(path).map(String::as_str),
                "generated source differs: {path}"
            );
        }
        let generated_originals = generation["original_generated_copies"].as_array().unwrap();
        assert_eq!(generated_originals.len(), 4);
        for original in generated_originals {
            let path = original.as_str().unwrap();
            assert!(originals.iter().any(|input| input["retained_path"] == path));
        }
        let headers = generation["new_generator_headers"].as_array().unwrap();
        assert_eq!(headers.len(), 2);
        for header in headers {
            let path = header.as_str().unwrap();
            assert!(outputs.contains_key(path));
            assert!(originals.iter().all(|input| input["path"] != path));
        }
        for claim in [
            "binary_signature_independently_verified",
            "upstream_original_tables_reproduced",
            "all_tree_behavioral_equivalence_proven",
            "os_network_denial_verified",
        ] {
            assert_eq!(generation[claim], serde_json::Value::Bool(false));
        }
        for claim in [
            "native_parser_execution_verified",
            "source_to_binary_reproduction_verified",
            "full_plsql_conformance_proven",
            "build_and_packaging_acceptance_verified",
        ] {
            assert_eq!(provenance[claim], serde_json::Value::Bool(false));
        }
        validate_vendored_tree(
            &root.join("third_party/tree-sitter-plsql-0.0.1"),
            grammar,
            sources,
        )
        .expect(
            "the original upstream tree, local grammar and generated sources are authenticated",
        );
    }

    #[test]
    fn apex_preserves_published_sources_and_qualifies_only_native_observations() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let bytes = fs::read(root.join(GRAMMAR_LOCK_PATH)).unwrap();
        let manifest: GrammarLock = toml::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "apex")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        assert_eq!(
            grammar.parser_sha256,
            "17c58f2ef3726022a7b24b5564dadf59d46641e24a1cddf725dc247986b9586e"
        );
        assert_eq!(grammar.abi, 15);
        assert_eq!(grammar.scanner_sha256, "none");
        assert_eq!(sources.files.len(), 54);
        assert_eq!(sources.parser_path.as_deref(), Some("apex/src/parser.c"));
        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-sfapex-3.0.1/SOURCE-PROVENANCE.json"
        ))
        .unwrap();
        let originals = provenance["published_source_originals"]["member_inventory"]
            .as_array()
            .unwrap();
        assert_eq!(originals.len(), 45);
        let mut total = 0u64;
        for original in originals {
            let retained = original["retained_path"].as_str().unwrap();
            assert_eq!(
                sources.files.get(retained).map(String::as_str),
                original["sha256"].as_str()
            );
            assert_eq!(original["retained_bytes_unmodified"], true);
            total += original["bytes"].as_u64().unwrap();
        }
        assert_eq!(total, 9_858_531);
        for claim in [
            "whole_language_qualified",
            "full_semantic_completeness_proven",
            "salesforce_compiler_or_runtime_qualified",
            "production_frontend_or_service_integration_qualified",
            "installed_mcp_qualified",
            "source_security_qualified",
            "legal_notice_completeness_qualified",
            "historical_java_ancestry_qualified",
            "empty_dependency_cache_qualified",
            "independently_blocked_network_build_qualified",
            "declared_msrv_1_90_verified",
        ] {
            assert_eq!(
                provenance["qualifications"][claim],
                serde_json::Value::Bool(false),
                "{claim}"
            );
        }
        validate_vendored_tree(
            &root.join("third_party/tree-sitter-sfapex-3.0.1"),
            grammar,
            sources,
        )
        .unwrap();
    }

    #[test]
    fn pascal_retains_published_inputs_and_two_preferred_spelling_repairs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "pascal")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        let directory = root.join("third_party/tree-sitter-pascal-0.10.2");
        assert!(grammar.crates_io_checksum.is_none());
        assert_eq!(
            grammar.vendored_source_sha256.as_deref(),
            Some("4ce8bf2245c875e3009bb978bc9888c558c5c17c740bdae89e78c3837e0be98a")
        );
        assert_eq!(grammar.abi, 14);
        assert_eq!(grammar.scanner_sha256, "none");
        assert_eq!(sources.files.len(), 28);
        assert_eq!(grammar.parser_sha256, sources.files["src/parser.c"]);
        let inventory: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-pascal-0.10.2/UPSTREAM-INVENTORY.json"
        ))
        .unwrap();
        assert_eq!(
            inventory["schema"],
            "rootlight.vendored-published-archive-inventory/1"
        );
        assert_eq!(inventory["member_count"], 15);
        assert_eq!(inventory["regular_file_count"], 15);
        assert_eq!(inventory["regular_file_bytes"], 3_976_845);
        let originals = inventory["original_members"].as_array().unwrap();
        assert_eq!(originals.len(), 15);
        let mut member_paths = BTreeSet::new();
        let mut retained_paths = BTreeSet::new();
        let mut original_bytes = 0u64;
        for original in originals {
            let path = original["path"].as_str().unwrap();
            let retained = original["retained_path"].as_str().unwrap();
            assert!(is_source_relative_path(path));
            assert!(is_source_relative_path(retained));
            assert!(member_paths.insert(path));
            assert!(retained_paths.insert(retained));
            assert_eq!(original["raw_mode"], 0o644);
            let expected = original["sha256"].as_str().unwrap();
            assert_eq!(
                sources.files.get(retained).map(String::as_str),
                Some(expected)
            );
            let bytes = fs::read(directory.join(retained)).unwrap();
            require_digest("retained Pascal original", &bytes, expected).unwrap();
            assert_eq!(u64::try_from(bytes.len()).unwrap(), original["bytes"]);
            original_bytes += u64::try_from(bytes.len()).unwrap();
        }
        assert_eq!(original_bytes, 3_976_845);
        assert!(originals.iter().any(|row| {
            row["path"] == "Cargo.toml" && row["retained_path"] == "Cargo.toml.published.orig"
        }));
        assert!(originals.iter().any(|row| {
            row["path"] == "Cargo.toml.orig" && row["retained_path"] == "Cargo.toml.orig"
        }));
        assert_ne!(
            sources.files["Cargo.toml.orig"],
            sources.files["Cargo.toml.published.orig"]
        );
        for (path_field, bytes_field, hash_field) in [
            (
                "original_archive",
                "original_archive_bytes",
                "original_archive_sha256",
            ),
            (
                "registry_metadata",
                "registry_metadata_bytes",
                "registry_metadata_sha256",
            ),
        ] {
            let path = inventory[path_field].as_str().unwrap();
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                inventory[hash_field].as_str()
            );
            assert_eq!(
                fs::metadata(directory.join(path)).unwrap().len(),
                inventory[bytes_field]
            );
        }
        for claim in [
            "archive_signature_verified",
            "authored_commit_equivalence_qualified",
            "legal_notice_completeness_qualified",
            "source_security_qualified",
            "empty_dependency_cache_qualified",
            "independently_blocked_network_build_qualified",
        ] {
            assert_eq!(inventory[claim], serde_json::Value::Bool(false), "{claim}");
        }
        let original_grammar = fs::read_to_string(directory.join("grammar.js.orig")).unwrap();
        let preferred_grammar = fs::read_to_string(directory.join("grammar.js")).unwrap();
        assert_eq!(original_grammar.matches("/seled/i").count(), 1);
        assert_eq!(original_grammar.matches("/safecal/i").count(), 1);
        assert_eq!(
            preferred_grammar,
            original_grammar
                .replace("/seled/i", "/sealed/i")
                .replace("/safecal/i", "/safecall/i")
        );
        let parser = fs::read_to_string(directory.join("src/parser.c")).unwrap();
        for declaration in [
            "#define LANGUAGE_VERSION 14",
            "#define STATE_COUNT 2715",
            "#define EXTERNAL_TOKEN_COUNT 0",
        ] {
            assert!(
                parser.lines().any(|line| line == declaration),
                "{declaration}"
            );
        }
        assert!(!sources.files.contains_key("src/scanner.c"));
        assert_eq!(
            sources
                .files
                .keys()
                .map(|path| fs::metadata(directory.join(path)).unwrap().len())
                .sum::<u64>(),
            8_031_546
        );
        validate_vendored_tree(&directory, grammar, sources)
            .expect("all original Pascal inputs and local static tables remain authenticated");
    }

    #[test]
    fn local_source_identity_requires_exactly_one_provenance_and_a_live_tree() {
        let (directory, mut grammar) = vendored_fixture();
        grammar.crates_io_checksum = None;
        assert!(matches!(
            validate_grammar(&grammar),
            Err(GrammarLockError::SourceProvenance { .. })
        ));
        grammar.vendored_source_sha256 =
            Some(vendored_source_digest(&grammar.vendored.as_ref().unwrap().files).unwrap());
        validate_grammar(&grammar).unwrap();
        validate_vendored_tree(
            directory.path(),
            &grammar,
            grammar.vendored.as_ref().unwrap(),
        )
        .unwrap();
        grammar.crates_io_checksum = Some(sha256_hex(b"archive"));
        assert!(matches!(
            validate_grammar(&grammar),
            Err(GrammarLockError::SourceProvenance { .. })
        ));
        grammar.crates_io_checksum = None;
        grammar.vendored = None;
        assert!(matches!(
            validate_grammar(&grammar),
            Err(GrammarLockError::SourceProvenance { .. })
        ));
    }

    #[test]
    fn source_manifest_digest_binds_paths_and_content_with_portable_framing() {
        let files = BTreeMap::from([("src/Î».bas".into(), sha256_hex(b"source"))]);
        // Independently computed from the documented UTF-8/NUL format, not TOML
        // serialization or host-native path separators.
        assert_eq!(
            vendored_source_digest(&files).unwrap(),
            "3cc9525c80eb1d734a3e307c9546bfc694f23a0f61fbfeef1d0a93c7d31d34a7"
        );
        let digest = vendored_source_digest(&files).unwrap();
        for changed in [
            BTreeMap::new(),
            BTreeMap::from([("src/other.bas".into(), sha256_hex(b"source"))]),
            BTreeMap::from([("src/Î».bas".into(), sha256_hex(b"changed"))]),
        ] {
            assert_ne!(vendored_source_digest(&changed).unwrap(), digest);
        }
        for path in [
            "", "/src/a", "../a", "src/./a", "src//a", "src\\a", "C:/a", "a\0b",
        ] {
            assert!(matches!(
                vendored_source_digest(&BTreeMap::from([(path.into(), sha256_hex(b"source"))])),
                Err(GrammarLockError::InvalidSourcePath(_))
            ));
        }
        assert!(vendored_source_digest(&BTreeMap::from([("a".into(), "invalid".into())])).is_err());
    }

    #[test]
    fn local_source_identity_rejects_rewritten_manifest_and_live_file_changes() {
        let (directory, mut grammar) = vendored_fixture();
        grammar.crates_io_checksum = None;
        let vendored = grammar.vendored.as_mut().unwrap();
        grammar.vendored_source_sha256 = Some(vendored_source_digest(&vendored.files).unwrap());
        let original = vendored
            .files
            .insert("LICENSE".into(), sha256_hex(b"changed"));
        assert!(matches!(
            validate_grammar(&grammar),
            Err(GrammarLockError::DigestMismatch {
                label: "vendored source manifest",
                ..
            })
        ));
        grammar
            .vendored
            .as_mut()
            .unwrap()
            .files
            .insert("LICENSE".into(), original.unwrap());
        fs::write(directory.path().join("LICENSE"), b"changed").unwrap();
        assert!(matches!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap()
            ),
            Err(GrammarLockError::DigestMismatch {
                label: "vendored grammar source",
                ..
            })
        ));
        fs::write(directory.path().join("LICENSE"), b"license").unwrap();
        fs::remove_file(directory.path().join("src/parser.c")).unwrap();
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap()
            )
            .is_err()
        );
        fs::write(directory.path().join("src/parser.c"), FIXTURE_PARSER).unwrap();
        fs::write(directory.path().join("extra.h"), b"extra").unwrap();
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn registry_admission_cannot_be_replaced_by_a_local_source_identity() {
        let baseline: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        for (index, grammar) in baseline.grammars.iter().enumerate() {
            if grammar.vendored.is_none() || grammar.crates_io_checksum.is_none() {
                continue;
            }
            let mut manifest: GrammarLock =
                toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
            let grammar = &mut manifest.grammars[index];
            grammar.crates_io_checksum = None;
            grammar.vendored_source_sha256 =
                Some(vendored_source_digest(&grammar.vendored.as_ref().unwrap().files).unwrap());
            validate_grammar(grammar).unwrap();
            assert!(
                matches!(
                    validate_manifest(&manifest),
                    Err(GrammarLockError::PackageEvidence { .. })
                ),
                "grammar index {index}"
            );
        }
    }

    #[test]
    fn cargo_lock_requires_one_exact_source_for_registry_and_local_grammars() {
        for vendored in [false, true] {
            for mutation in [
                "none",
                "missing",
                "duplicate",
                "source",
                "checksum",
                "version",
            ] {
                let (_directory, mut grammar) = vendored_fixture();
                if vendored {
                    grammar.crates_io_checksum = None;
                    grammar.vendored_source_sha256 = Some(
                        vendored_source_digest(&grammar.vendored.as_ref().unwrap().files).unwrap(),
                    );
                } else {
                    grammar.vendored = None;
                }
                let mut manifest: GrammarLock =
                    toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
                let mut lock: CargoLock = toml::from_str(include_str!("../../Cargo.lock")).unwrap();
                let package = || LockedPackage {
                    name: grammar.crate_name.clone(),
                    version: grammar.crate_version.clone(),
                    checksum: grammar.crates_io_checksum.clone(),
                    source: (!vendored).then(|| CRATES_IO_SOURCE.to_owned()),
                };
                if mutation != "missing" {
                    lock.package.push(package());
                }
                if mutation == "duplicate" {
                    lock.package.push(package());
                }
                let last = lock.package.last_mut().unwrap();
                match mutation {
                    "source" => last.source = Some("registry+https://example.invalid/index".into()),
                    "checksum" => last.checksum = Some(sha256_hex(b"unexpected")),
                    "version" => last.version = "2.0.0".into(),
                    _ => {}
                }
                manifest.grammars = vec![grammar];
                let result = validate_locked_packages(&lock, &manifest);
                if mutation == "none" {
                    result.unwrap();
                } else {
                    assert!(
                        matches!(result, Err(GrammarLockError::CargoLockChecksum { .. })),
                        "vendored={vendored}, mutation={mutation}"
                    );
                }
            }
        }
    }

    #[test]
    fn vendored_tree_rejects_modified_missing_and_extra_inputs() {
        let (directory, grammar) = vendored_fixture();
        let vendored = grammar
            .vendored
            .as_ref()
            .expect("fixture has local evidence");
        validate_vendored_tree(directory.path(), &grammar, vendored).expect("exact tree passes");
        fs::write(directory.path().join("src/scanner.c"), b"changed").expect("mutation writes");
        assert!(matches!(
            validate_vendored_tree(directory.path(), &grammar, vendored),
            Err(GrammarLockError::DigestMismatch { .. })
        ));
        fs::write(directory.path().join("src/scanner.c"), b"scanner").expect("scanner restores");
        fs::remove_file(directory.path().join("src/parser.c")).expect("fixture parser removes");
        assert!(validate_vendored_tree(directory.path(), &grammar, vendored).is_err());
        fs::write(directory.path().join("src/parser.c"), FIXTURE_PARSER).expect("parser restores");
        fs::write(directory.path().join("src/extra.h"), b"extra")
            .expect("unreviewed header writes");
        assert!(validate_vendored_tree(directory.path(), &grammar, vendored).is_err());
    }

    #[test]
    fn vendored_tree_binds_file_hashes_to_the_published_descriptor() {
        let (directory, mut grammar) = vendored_fixture();
        grammar.scanner_sha256 = sha256_hex(b"different descriptor");
        assert!(matches!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().expect("local evidence")
            ),
            Err(GrammarLockError::PackageEvidence { .. })
        ));
    }

    #[test]
    fn vendored_tree_accepts_explicit_nested_sources_but_not_path_aliases() {
        let (directory, mut grammar) = vendored_fixture();
        fs::create_dir(directory.path().join("block")).unwrap();
        fs::rename(
            directory.path().join("src"),
            directory.path().join("block/src"),
        )
        .unwrap();
        let vendored = grammar.vendored.as_mut().unwrap();
        for path in ["src/parser.c", "src/scanner.c", "src/tree_sitter/parser.h"] {
            let digest = vendored.files.remove(path).unwrap();
            vendored.files.insert(format!("block/{path}"), digest);
        }
        vendored.parser_path = Some("block/src/parser.c".into());
        vendored.scanner_path = Some("block/src/scanner.c".into());
        validate_vendored_tree(
            directory.path(),
            &grammar,
            grammar.vendored.as_ref().unwrap(),
        )
        .unwrap();
        for path in [
            "",
            "../block/src/parser.c",
            "/block/src/parser.c",
            "block/./src/parser.c",
            "block//src/parser.c",
            "block\\src\\parser.c",
            "C:/block/src/parser.c",
            "block/src/scanner.c",
        ] {
            grammar.vendored.as_mut().unwrap().parser_path = Some(path.into());
            assert!(
                validate_vendored_tree(
                    directory.path(),
                    &grammar,
                    grammar.vendored.as_ref().unwrap()
                )
                .is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn vendored_license_path_requires_exact_file_digest_and_relative_identity() {
        let (directory, mut grammar) = vendored_fixture();
        fs::rename(
            directory.path().join("LICENSE"),
            directory.path().join("LICENSE-MIT"),
        )
        .unwrap();
        let vendored = grammar.vendored.as_mut().unwrap();
        let digest = vendored.files.remove("LICENSE").unwrap();
        vendored.files.insert("LICENSE-MIT".into(), digest);
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap()
            )
            .is_err()
        );
        grammar.vendored.as_mut().unwrap().license_path = Some("LICENSE-MIT".into());
        validate_vendored_tree(
            directory.path(),
            &grammar,
            grammar.vendored.as_ref().unwrap(),
        )
        .unwrap();
        for path in [
            "",
            "../LICENSE-MIT",
            "/LICENSE-MIT",
            "./LICENSE-MIT",
            "C:/LICENSE-MIT",
            "LICENSE",
            "src/parser.c",
        ] {
            grammar.vendored.as_mut().unwrap().license_path = Some(path.into());
            assert!(
                validate_vendored_tree(
                    directory.path(),
                    &grammar,
                    grammar.vendored.as_ref().unwrap()
                )
                .is_err(),
                "{path}"
            );
        }
        grammar.vendored.as_mut().unwrap().license_path = Some("LICENSE-MIT".into());
        grammar.license_sha256 = sha256_hex(b"different license");
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn scannerless_vendor_requires_both_absent_file_and_absent_descriptor() {
        let (directory, mut grammar) = vendored_fixture();
        grammar.scanner_sha256 = "none".into();
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().expect("evidence")
            )
            .is_err()
        );
        grammar
            .vendored
            .as_mut()
            .expect("evidence")
            .files
            .remove("src/scanner.c");
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().expect("evidence")
            )
            .is_err(),
            "undeclared scanner still exists"
        );
        fs::remove_file(directory.path().join("src/scanner.c")).expect("remove fixture scanner");
        validate_vendored_tree(
            directory.path(),
            &grammar,
            grammar.vendored.as_ref().expect("evidence"),
        )
        .expect("scannerless grammar passes");
        grammar.scanner_sha256 = sha256_hex(b"scanner");
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().expect("evidence")
            )
            .is_err(),
            "declared scanner is missing"
        );
    }

    #[test]
    fn authenticated_native_tables_still_reject_uint16_narrowing_with_typed_source() {
        use std::error::Error as _;

        for (name, kind) in [
            ("ACTIONS", native_table_bounds::IndexMacro::Actions),
            ("STATE", native_table_bounds::IndexMacro::State),
            ("SHIFT", native_table_bounds::IndexMacro::Shift),
            ("SHIFT_REPEAT", native_table_bounds::IndexMacro::ShiftRepeat),
        ] {
            let (directory, mut grammar) = vendored_fixture();
            let parser = format!("/* generated table */\n{name}(70779)");
            replace_fixture_input(
                directory.path(),
                &mut grammar,
                "src/parser.c",
                parser.as_bytes(),
            );
            let error = validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap(),
            )
            .unwrap_err();
            let expected = native_table_bounds::NativeTableBoundsError::IndexOutOfRange {
                kind,
                offset: parser.find(name).unwrap(),
                value: 70779,
                maximum: u16::MAX,
            };
            assert_eq!(
                error
                    .source()
                    .unwrap()
                    .downcast_ref::<native_table_bounds::NativeTableBoundsError>(),
                Some(&expected)
            );
            assert!(matches!(error, GrammarLockError::NativeTableBounds {
                package, parser_path, header_path, source,
            } if package == "tree-sitter-fixture"
                && parser_path == directory.path().join("src/parser.c")
                && header_path == directory.path().join("src/tree_sitter/parser.h")
                && source == expected));
        }
    }

    #[test]
    fn authenticated_native_macro_expressions_are_not_literal_width_proofs() {
        for parser in ["ACTIONS(offset)", "ACTIONS(1+2)", "SHIFT(-1)", "STATE()"] {
            let (directory, mut grammar) = vendored_fixture();
            replace_fixture_input(
                directory.path(),
                &mut grammar,
                "src/parser.c",
                parser.as_bytes(),
            );
            assert!(matches!(
                validate_vendored_tree(
                    directory.path(),
                    &grammar,
                    grammar.vendored.as_ref().unwrap(),
                ),
                Err(GrammarLockError::NativeTableBounds {
                    source: native_table_bounds::NativeTableBoundsError::InvalidLiteral { .. },
                    ..
                })
            ));
        }
    }

    #[test]
    fn native_width_checks_do_not_replace_exact_source_authentication() {
        let (directory, grammar) = vendored_fixture();
        fs::write(directory.path().join("src/parser.c"), b"ACTIONS(70779)").unwrap();
        assert!(matches!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap(),
            ),
            Err(GrammarLockError::DigestMismatch {
                label: "vendored grammar source",
                ..
            })
        ));
    }

    #[test]
    fn native_header_requires_the_authenticated_selected_parser_sibling() {
        let (directory, mut grammar) = vendored_fixture();
        fs::remove_file(directory.path().join("src/tree_sitter/parser.h")).unwrap();
        fs::create_dir(directory.path().join("tree_sitter")).unwrap();
        fs::write(
            directory.path().join("tree_sitter/parser.h"),
            FIXTURE_HEADER,
        )
        .unwrap();
        let files = &mut grammar.vendored.as_mut().unwrap().files;
        let digest = files.remove("src/tree_sitter/parser.h").unwrap();
        files.insert("tree_sitter/parser.h".into(), digest);
        assert!(matches!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap(),
            ),
            Err(GrammarLockError::PackageEvidence { .. })
        ));
    }

    #[test]
    fn native_header_cannot_be_a_directory_even_with_an_inventory_entry() {
        let (directory, mut grammar) = vendored_fixture();
        let path = directory.path().join("src/tree_sitter/parser.h");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("other.h"), FIXTURE_HEADER).unwrap();
        grammar.vendored.as_mut().unwrap().files.insert(
            "src/tree_sitter/parser.h/other.h".into(),
            sha256_hex(FIXTURE_HEADER),
        );
        assert!(matches!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap(),
            ),
            Err(GrammarLockError::PackageEvidence { .. })
        ));
    }

    #[test]
    fn authenticated_wider_header_does_not_redefine_the_runtime_abi() {
        for (original, replacement, member) in [
            (
                "typedef uint16_t TSStateId",
                "typedef uint32_t TSStateId",
                native_table_bounds::AbiMember::StateId,
            ),
            (
                "const uint16_t *parse_table",
                "const uint32_t *parse_table",
                native_table_bounds::AbiMember::ParseTable,
            ),
            (
                "const uint16_t *small_parse_table",
                "const uint32_t *small_parse_table",
                native_table_bounds::AbiMember::SmallParseTable,
            ),
        ] {
            let (directory, mut grammar) = vendored_fixture();
            let header = std::str::from_utf8(FIXTURE_HEADER)
                .unwrap()
                .replace(original, replacement);
            replace_fixture_input(
                directory.path(),
                &mut grammar,
                "src/tree_sitter/parser.h",
                header.as_bytes(),
            );
            assert!(matches!(validate_vendored_tree(
                directory.path(), &grammar, grammar.vendored.as_ref().unwrap(),
            ), Err(GrammarLockError::NativeTableBounds {
                source: native_table_bounds::NativeTableBoundsError::HeaderDeclaration { member: observed }, ..
            }) if observed == member));
        }
    }

    #[test]
    fn authenticated_parser_and_header_must_be_utf8_before_lexical_validation() {
        for path in ["src/parser.c", "src/tree_sitter/parser.h"] {
            let (directory, mut grammar) = vendored_fixture();
            replace_fixture_input(directory.path(), &mut grammar, path, b"\xff");
            assert!(matches!(validate_vendored_tree(
                directory.path(), &grammar, grammar.vendored.as_ref().unwrap(),
            ), Err(GrammarLockError::Utf8 { path: observed, .. }) if observed == directory.path().join(path)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_parser_and_header_cannot_follow_authenticated_external_symlinks() {
        for path in ["src/parser.c", "src/tree_sitter/parser.h"] {
            let (directory, grammar) = vendored_fixture();
            let external = tempfile::tempdir().unwrap();
            let bytes = fs::read(directory.path().join(path)).unwrap();
            let target = external.path().join("source");
            fs::write(&target, bytes).unwrap();
            fs::remove_file(directory.path().join(path)).unwrap();
            std::os::unix::fs::symlink(target, directory.path().join(path)).unwrap();
            assert!(matches!(
                validate_vendored_tree(
                    directory.path(),
                    &grammar,
                    grammar.vendored.as_ref().unwrap(),
                ),
                Err(GrammarLockError::PackageEvidence { .. })
            ));
        }
    }

    fn replace_fixture_input(
        directory: &Path,
        grammar: &mut GrammarEvidence,
        path: &str,
        bytes: &[u8],
    ) {
        fs::write(directory.join(path), bytes).unwrap();
        let digest = sha256_hex(bytes);
        grammar
            .vendored
            .as_mut()
            .unwrap()
            .files
            .insert(path.into(), digest.clone());
        if path == "src/parser.c" {
            grammar.parser_sha256 = digest;
        }
    }

    fn vendored_fixture() -> (tempfile::TempDir, GrammarEvidence) {
        let directory = tempfile::tempdir().expect("fixture directory");
        fs::create_dir_all(directory.path().join("src/tree_sitter")).expect("source directory");
        let mut files = BTreeMap::new();
        for (path, bytes) in [
            ("Cargo.toml", b"manifest".as_slice()),
            ("src/parser.c", FIXTURE_PARSER),
            ("src/tree_sitter/parser.h", FIXTURE_HEADER),
            ("src/scanner.c", b"scanner"),
            ("LICENSE", b"license"),
        ] {
            fs::write(directory.path().join(path), bytes).expect("fixture source writes");
            files.insert(path.to_owned(), sha256_hex(bytes));
        }
        let grammar = GrammarEvidence {
            language: "fixture".into(),
            crate_name: "tree-sitter-fixture".into(),
            crate_version: "1.0.0".into(),
            crates_io_checksum: Some(sha256_hex(b"archive")),
            vendored_source_sha256: None,
            repository: "https://example.invalid/grammar".into(),
            tag: "v1.0.0".into(),
            commit: "fixture".into(),
            parser_sha256: sha256_hex(FIXTURE_PARSER),
            scanner_sha256: sha256_hex(b"scanner"),
            abi: 15,
            analysis_tier: "D".into(),
            license: "MIT".into(),
            license_source: "LICENSE".into(),
            license_sha256: sha256_hex(b"license"),
            modifications: "fixture".into(),
            test_corpus: "fixture".into(),
            msrv: "1.90".into(),
            offline_behavior: "fixture".into(),
            capabilities: vec!["fixture".into()],
            semantic_depth: "fixture".into(),
            audit_notes: Vec::new(),
            vendored: Some(VendoredGrammarEvidence {
                manifest_path: "third_party/grammar/Cargo.toml".into(),
                parser_path: None,
                scanner_path: None,
                license_path: None,
                files,
            }),
        };
        (directory, grammar)
    }

    #[test]
    fn digest_validation_rejects_truncated_and_uppercase_values() {
        for invalid in [
            "abcd",
            "A25fcae685241c79da6ccba18d38a6f81e963cd2cad75434a65b28b50246ac8c",
        ] {
            assert!(matches!(
                validate_sha256("fixture", invalid),
                Err(GrammarLockError::InvalidDigest { label: "fixture" })
            ));
        }
    }

    #[test]
    fn dependency_set_rejects_an_unreviewed_tree_sitter_package() {
        let mut dependencies = audited_dependency_fixtures();
        dependencies.push(dependency_fixture("tree-sitter-unreviewed", "1.0.0", &[]));

        assert!(matches!(
            validate_tree_sitter_dependencies(&dependencies, Path::new("/workspace")),
            Err(GrammarLockError::DependencySet)
        ));
    }

    #[test]
    fn dependency_profile_rejects_non_normal_optional_and_non_crates_io_forms() {
        let baseline = dependency_fixture("tree-sitter-rust", "0.24.2", &[]);
        let mut profiles = Vec::new();

        let mut development = baseline.clone();
        development.kind = DependencyKind::Development;
        profiles.push(development);

        let mut optional = baseline.clone();
        optional.optional = true;
        profiles.push(optional);

        let mut targeted = baseline.clone();
        targeted.target = Some(
            "cfg(windows)"
                .parse()
                .expect("test target expression is valid"),
        );
        profiles.push(targeted);

        let mut path = baseline.clone();
        path.source = None;
        profiles.push(path);

        let alternate_registry = dependency_fixture_with_source(
            "tree-sitter-rust",
            "0.24.2",
            &[],
            Some("registry+https://example.invalid/index"),
            Some("https://example.invalid/index"),
        );
        profiles.push(alternate_registry);

        for dependency in profiles {
            assert!(matches!(
                validate_dependency_profile(&dependency, "0.24.2", None),
                Err(GrammarLockError::DependencyProfile { .. })
            ));
        }
    }

    fn audited_dependency_fixtures() -> Vec<Dependency> {
        EXPECTED_PACKAGES
            .iter()
            .map(|(name, version, _)| {
                let features = if *name == "tree-sitter" {
                    &["std"][..]
                } else {
                    &[][..]
                };
                dependency_fixture(name, version, features)
            })
            .chain(
                EXPECTED_LOCAL_PACKAGES
                    .iter()
                    .map(|(name, version, directory, _)| {
                        let mut dependency =
                            dependency_fixture_with_source(name, version, &[], None, None);
                        dependency.path = Some(format!("/workspace/{directory}").into());
                        if *name == "tree-sitter-scheme-native-local" {
                            dependency.rename = Some("tree-sitter-scheme".into());
                        }
                        dependency
                    }),
            )
            .collect()
    }

    #[test]
    fn scheme_dependency_requires_its_reviewed_alias_without_default_features() {
        let dependencies = audited_dependency_fixtures();
        let baseline = dependencies
            .iter()
            .find(|dependency| dependency.name == "tree-sitter-scheme-native-local")
            .unwrap();
        let path = Path::new("/workspace/third_party/tree-sitter-scheme-0.24.7-1");
        validate_dependency_profile(baseline, "0.24.7-1", Some(path)).unwrap();
        for alias in [None, Some("unreviewed")] {
            let mut dependency = baseline.clone();
            dependency.rename = alias.map(str::to_owned);
            assert!(matches!(
                validate_dependency_profile(&dependency, "0.24.7-1", Some(path)),
                Err(GrammarLockError::DependencyProfile { .. })
            ));
        }
        let mut dependency = baseline.clone();
        dependency.uses_default_features = true;
        assert!(matches!(
            validate_dependency_profile(&dependency, "0.24.7-1", Some(path)),
            Err(GrammarLockError::DependencyProfile { .. })
        ));
    }

    #[test]
    fn local_dependency_requires_the_reviewed_path_and_exact_profile() {
        let dependencies = audited_dependency_fixtures();
        validate_tree_sitter_dependencies(&dependencies, Path::new("/workspace")).unwrap();
        let baseline = dependencies
            .iter()
            .find(|dependency| dependency.name == "tree-sitter-vba")
            .unwrap();
        let path = Path::new("/workspace/third_party/tree-sitter-vba-0.14.1");
        for mutation in [
            "missing_path",
            "different_path",
            "registry",
            "optional",
            "features",
            "version",
            "renamed",
            "target",
            "development",
            "default_features",
        ] {
            let mut dependency = baseline.clone();
            match mutation {
                "missing_path" => dependency.path = None,
                "different_path" => dependency.path = Some("/workspace/unreviewed".into()),
                "registry" => dependency = dependency_fixture("tree-sitter-vba", "0.14.1", &[]),
                "optional" => dependency.optional = true,
                "features" => dependency.features.push("unreviewed".into()),
                "version" => dependency.req = ">=0.14.1".parse().unwrap(),
                "renamed" => dependency.rename = Some("replacement".into()),
                "target" => dependency.target = Some("cfg(windows)".parse().unwrap()),
                "development" => dependency.kind = DependencyKind::Development,
                "default_features" => dependency.uses_default_features = true,
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    validate_dependency_profile(&dependency, "0.14.1", Some(path)),
                    Err(GrammarLockError::DependencyProfile { .. })
                ),
                "{mutation}"
            );
        }
    }

    fn dependency_fixture(name: &str, version: &str, features: &[&str]) -> Dependency {
        dependency_fixture_with_source(name, version, features, Some(CRATES_IO_SOURCE), None)
    }

    fn dependency_fixture_with_source(
        name: &str,
        version: &str,
        features: &[&str],
        source: Option<&str>,
        registry: Option<&str>,
    ) -> Dependency {
        serde_json::from_value(json!({
            "name": name,
            "source": source,
            "req": format!("={version}"),
            "kind": null,
            "rename": null,
            "optional": false,
            "uses_default_features": false,
            "features": features,
            "target": null,
            "registry": registry,
        }))
        .expect("test dependency metadata is valid")
    }
}

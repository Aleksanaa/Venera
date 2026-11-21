{ lib, rustPlatform, stdenvNoCC, runCommand, cacert, nodejs_22, yarn-berry, makeWrapper, libvirt, procps }:
let
  yarnCache = runCommand "venera-debug-yarn-cache" {
    nativeBuildInputs = [ yarn-berry cacert ];
    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    outputHash = "sha256-WYEYxZAV1rl/Bcq4RpJlnUsU0304qIqStzoGvGR3KnA=";
  } ''
    export HOME=$TMPDIR YARN_ENABLE_GLOBAL_CACHE=false YARN_CACHE_FOLDER=$out
    cp ${./web/package.json} package.json
    cp ${./web/yarn.lock} yarn.lock
    cp ${./web/.yarnrc.yml} .yarnrc.yml
    yarn install --immutable --mode=skip-build
  '';
  web = stdenvNoCC.mkDerivation {
    name = "venera-debug-web";
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [ ./web ../docs/venera.svg ];
    };
    sourceRoot = "source/debug-channel/web";
    nativeBuildInputs = [ nodejs_22 yarn-berry ];
    buildPhase = ''
      export HOME=$TMPDIR YARN_ENABLE_NETWORK=false YARN_ENABLE_GLOBAL_CACHE=false YARN_CACHE_FOLDER=${yarnCache}
      yarn install --immutable --immutable-cache --mode=skip-build
      yarn build
    '';
    installPhase = "cp -r dist $out";
  };
in rustPlatform.buildRustPackage {
  pname = "venera-debug-host";
  version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./guest ./host ./protocol ];
  };
  cargoLock.lockFile = ./Cargo.lock;
  cargoBuildFlags = [ "-p" "venera-debug-host" ];
  cargoTestFlags = [ "-p" "venera-debug-host" ];
  buildFeatures = [ "venera-debug-host/web" ];
  nativeBuildInputs = [ makeWrapper ];
  nativeCheckInputs = [ procps ];
  postInstall = ''
    mkdir -p $out/share/venera
    cp -r ${web} $out/share/venera/web
    wrapProgram $out/bin/venera-debug-host --prefix PATH : ${lib.makeBinPath [ libvirt ]}
  '';
  meta.mainProgram = "venera-debug-host";
}

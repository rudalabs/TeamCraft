#!/usr/bin/env bash
# Seeds demo data into a fresh TeamCraft and captures the README screenshots.
#
# Expects a running TeamCraft at $BASE_URL with an EMPTY database, listening on
# localhost (the first-start /setup page creates the administrator), plus
# curl, jq, base64, Google Chrome and a matching chromedriver. Builds nothing.
# Used by .github/workflows/screenshots.yml; see CONTRIBUTING.md, "Updating
# the README screenshots".
#
#   BASE_URL=http://127.0.0.1:8080 OUT_DIR=/tmp/shots scripts/screenshots.sh
#
# Environment (all optional):
#   BASE_URL          TeamCraft address               (http://127.0.0.1:8080)
#   OUT_DIR           where the PNGs go                (media/screenshots)
#   FIXTURES          NIST CAD files                   (tests/fixtures/cad/nist)
#   CHROMEDRIVER      chromedriver binary              (PATH, then $CHROMEWEBDRIVER)
#   CHROME_BIN        Chrome binary                    (chromedriver's default)
#   WD_PORT           port for chromedriver            (9515)
#   SCREENSHOT_SCALE  device pixel ratio of desktop shots (1)
#
# The passwords and API tokens are generated per run, kept in a private
# temporary directory and never printed (on GitHub Actions they are also
# masked).
set -euo pipefail

BASE_URL=${BASE_URL:-http://127.0.0.1:8080}
BASE_URL=${BASE_URL%/}
OUT_DIR=${OUT_DIR:-media/screenshots}
FIXTURES=${FIXTURES:-tests/fixtures/cad/nist}
WD_PORT=${WD_PORT:-9515}
SCALE=${SCREENSHOT_SCALE:-1}
WD="http://127.0.0.1:${WD_PORT}"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
die() {
    log "ERROR: $*"
    exit 1
}

for tool in curl jq base64 mktemp; do
    command -v "$tool" >/dev/null || die "'$tool' is required"
done
[[ -d $FIXTURES ]] || die "fixture folder '$FIXTURES' not found (run from the repository root or set FIXTURES)"

WORK=$(mktemp -d)
chmod 700 "$WORK"
DRIVER_PID=
SESSION=
cleanup() {
    local status=$?
    if [[ -n $SESSION ]]; then
        curl -s --max-time 10 -X DELETE "$WD/session/$SESSION" >/dev/null 2>&1 || true
    fi
    if [[ -n $DRIVER_PID ]]; then
        kill "$DRIVER_PID" 2>/dev/null || true
        wait "$DRIVER_PID" 2>/dev/null || true
    fi
    if [[ $status -ne 0 && -s $WORK/chromedriver.log ]]; then
        log "last lines of the chromedriver log:"
        tail -n 20 "$WORK/chromedriver.log" >&2 || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

# Keeps a generated secret out of the GitHub Actions log.
mask() {
    if [[ ${GITHUB_ACTIONS:-} == true ]]; then
        printf '::add-mask::%s\n' "$1"
    fi
}

new_password() {
    local pw
    pw=$(head -c 24 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | head -c 24)
    [[ ${#pw} -ge 16 ]] || die "could not generate a password"
    mask "$pw"
    # No trailing newline: curl's name@file reads the file verbatim.
    printf '%s' "$pw" >"$WORK/pw-$1"
}

# ---------------------------------------------------------------------------
# HTTP helpers
# ---------------------------------------------------------------------------

# web WHO PATH CURL-ARGS… – request with WHO's cookie jar; the body lands in
# $WORK/page, the status code is printed. Redirects are not followed.
web() {
    local who=$1 path=$2
    shift 2
    curl -sS --max-time 120 -b "$WORK/jar-$who" -c "$WORK/jar-$who" \
        -o "$WORK/page" -w '%{http_code}' "$@" "$BASE_URL$path" ||
        die "request to $path failed"
}

# api WHO METHOD PATH CURL-ARGS… – REST call with WHO's API token; prints the
# body and stops the script with the error message on HTTP >= 400.
api() {
    local who=$1 method=$2 path=$3 code
    shift 3
    local retry=()
    [[ $method == GET ]] && retry=(--retry 3 --retry-delay 2)
    code=$(curl -sS --max-time 300 "${retry[@]}" -X "$method" -H "@$WORK/auth-$who" \
        -o "$WORK/api" -w '%{http_code}' "$@" "$BASE_URL/api/v1$path") ||
        die "$method $path: request failed"
    if [[ $code -ge 400 ]]; then
        die "$method $path -> HTTP $code: $(jq -r '.error.message? // .' "$WORK/api" 2>/dev/null | head -c 400)"
    fi
    cat "$WORK/api"
}

# japi WHO METHOD PATH JSON – REST call with a JSON body.
japi() {
    api "$1" "$2" "$3" -H 'Content-Type: application/json' --data-binary "$4"
}

# Signs WHO in through the login form (curl) and creates a short-lived API token.
api_token() {
    local who=$1 code token
    code=$(web "$who" /account/tokens --data-urlencode name=readme-screenshots \
        --data-urlencode expires_days=1)
    [[ $code == 200 ]] || die "creating an API token for $who failed (HTTP $code)"
    token=$(grep -oE 'tc_pat_[0-9a-f]+' "$WORK/page" | head -n 1 || true)
    [[ -n $token ]] || die "the account page showed no new API token for $who"
    mask "$token"
    printf 'Authorization: Bearer %s\n' "$token" >"$WORK/auth-$who"
    rm -f "$WORK/page"
}

login_web() {
    local who=$1 code
    code=$(web "$who" /login --data-urlencode "username=$who" \
        --data-urlencode "password@$WORK/pw-$who" --data-urlencode next=/)
    [[ $code == 303 || $code == 302 ]] || die "login of $who failed (HTTP $code)"
}

wait_for_server() {
    local i
    for ((i = 0; i < 60; i++)); do
        if curl -fsS --max-time 5 "$BASE_URL/readyz" >/dev/null 2>&1; then
            return 0
        fi
        sleep 2
    done
    die "TeamCraft does not answer on $BASE_URL/readyz"
}

# ---------------------------------------------------------------------------
# Demo data
# ---------------------------------------------------------------------------

declare -A NUM REV

# item KEY TYPE NAME JSON-EXTRAS – creates an item in ROVER as admin (it starts
# checked out by its creator).
item() {
    local key=$1 type=$2 name=$3 extra=${4:-'{}'} body out
    body=$(jq -nc --arg t "$type" --arg n "$name" --argjson x "$extra" \
        '{item_type: $t, name: $n, project: "ROVER"} + $x')
    out=$(japi admin POST /items "$body")
    NUM[$key]=$(jq -r '.item_number' <<<"$out")
    REV[$key]=$(jq -r '.id' <<<"$out")
    log "  ${NUM[$key]}  $name"
}

# attach KEY FILE [NAME] [COMMENT] – uploads a file into the item's latest
# revision and prints the new file id.
attach() {
    local key=$1 path=$2 name=${3:-} comment=${4:-Initial upload}
    [[ -f $path ]] || die "fixture '$path' is missing"
    [[ -n $name ]] || name=$(basename "$path")
    api admin POST "/items/${NUM[$key]}/revisions/latest/files" \
        -F "file=@\"$path\";filename=\"$name\"" -F "comment=$comment" | jq -r '.[0].id'
}

bom_line() {
    local parent=$1 child=$2 qty=$3 find=$4 notes=${5:-}
    japi admin POST "/items/${NUM[$parent]}/revisions/latest/bom" \
        "$(jq -nc --arg c "${NUM[$child]}" --argjson q "$qty" --argjson f "$find" --arg n "$notes" \
            '{child: $c, quantity: $q, find_number: $f, notes: $n}')" >/dev/null
}

checkin() { api "${2:-admin}" POST "/items/${NUM[$1]}/revisions/latest/checkin" >/dev/null; }
checkout() { api "${2:-admin}" POST "/items/${NUM[$1]}/revisions/latest/checkout" >/dev/null; }

seed() {
    local code
    log "creating the administrator through the first-start page"
    new_password admin
    code=$(web admin /setup --data-urlencode username=admin \
        --data-urlencode "display_name=Alex Morgan" \
        --data-urlencode "password@$WORK/pw-admin" \
        --data-urlencode "password_repeat@$WORK/pw-admin")
    if [[ $code == 403 ]]; then
        die "the first-start setup is closed – the database must be empty and the server must listen on localhost"
    fi
    [[ $code == 303 || $code == 302 ]] || die "first-start setup failed (HTTP $code)"
    api_token admin

    log "creating the users mchen and jlopez"
    local user name
    for user in mchen:"Mia Chen" jlopez:"Jonas Lopez"; do
        name=${user#*:}
        user=${user%%:*}
        new_password "$user"
        code=$(web admin /admin/users --data-urlencode "username=$user" \
            --data-urlencode "display_name=$name" \
            --data-urlencode "email=$user@example.org" --data-urlencode role=editor \
            --data-urlencode "password@$WORK/pw-$user")
        [[ $code == 303 || $code == 302 ]] || die "creating $user failed (HTTP $code)"
        # A validation error also redirects (with a flash message); the
        # login below then fails with a clear message.
        login_web "$user"
        api_token "$user"
    done

    log "creating the project group ROVER"
    PROJECT_ID=$(japi admin POST /projects \
        '{"code":"ROVER","name":"Rover sensor mast","description":"Sensor mast of the field rover: electronics enclosure, brackets and fasteners."}' |
        jq -r '.id')
    japi admin POST /projects/ROVER/members '{"username":"mchen","manager":true}' >/dev/null
    japi admin POST /projects/ROVER/members '{"username":"jlopez"}' >/dev/null

    log "creating items"
    local mtc=$FIXTURES/mtc-assembly
    item mast assembly "Sensor mast assembly" '{"description":"Top-level assembly of the rover sensor mast."}'
    item enclosure assembly "Electronics enclosure" '{"description":"Sealed enclosure for the sensor electronics (NIST MTC CRADA assembly)."}'
    item box part "Enclosure box" '{"material":"Aluminium 6061-T6","mass_kg":1.42,"unit_cost":86.5}'
    item cover part "Enclosure cover" '{"material":"Aluminium 6061-T6","mass_kg":0.64,"unit_cost":41.0}'
    item plate part "Base plate" '{"material":"Aluminium 5083","mass_kg":0.95,"unit_cost":28.0}'
    item window part "Polycarbonate window" '{"material":"Polycarbonate, UV stabilised","mass_kg":0.12,"unit_cost":9.8}'
    item shcs_s part "Socket head cap screw 91274A118" '{"material":"Alloy steel, black oxide","mass_kg":0.004,"unit_cost":0.18}'
    item shcs_l part "Socket head cap screw 91274A141" '{"material":"Alloy steel, black oxide","mass_kg":0.006,"unit_cost":0.22}'
    item fhcs part "Flat-head socket cap screw 91304A112" '{"material":"Alloy steel, blue coated","mass_kg":0.003,"unit_cost":0.25}'
    item nut part "Hex nut 90591A141" '{"material":"Zinc-plated steel","mass_kg":0.002,"unit_cost":0.05}'
    item bracket part "Mounting bracket" '{"material":"Aluminium 7075-T6","mass_kg":0.38,"unit_cost":64.0,"description":"Machined bracket that holds the enclosure on the mast tube (NIST CTC-01)."}'
    item clamp part "Cable clamp" '{"material":"Stainless steel 1.4301","mass_kg":0.05,"unit_cost":3.4,"description":"Clamp for the sensor cable harness (NIST FTC-11)."}'

    log "uploading CAD files"
    attach mast "$mtc/nx/NIST mtc crada assembly.prt" >/dev/null
    attach mast "$mtc/nx/NIST mtc crada assembly.jt" >/dev/null
    attach enclosure "$mtc/solidworks/nist_mtc_crada_assembly_rev-D.SLDASM" >/dev/null
    attach box "$mtc/solidworks/nist_mtc_crada_box_rev-D.SLDPRT" >/dev/null
    attach box "$mtc/nx/NIST mtc crada box.prt" >/dev/null
    attach cover "$mtc/solidworks/nist_mtc_crada_cover_rev-B.SLDPRT" >/dev/null
    attach cover "$mtc/solidworks/nist_mtc_crada_cover_rev-B.SAT" >/dev/null
    attach plate "$mtc/solidworks/nist_mtc_crada_plate_rev-A.SLDPRT" >/dev/null
    attach window "$mtc/solidworks/nist_mtc_crada_polycarb-window.SLDPRT" >/dev/null
    attach shcs_s "$mtc/solidworks/91274A118_COATED ALLOY STEEL SOCKET HEAD CAP SCREW.SLDPRT" >/dev/null
    attach shcs_l "$mtc/solidworks/91274A141_COATED ALLOY STEEL SOCKET HEAD CAP SCREW.SLDPRT" >/dev/null
    attach fhcs "$mtc/solidworks/91304A112_BLUE FLAT-HEAD SOCKET CAP SCREW.SLDPRT" >/dev/null
    attach nut "$mtc/solidworks/90591A141_ZINC PLATED STEEL HEX NUT.SLDPRT" >/dev/null
    STEP_FILE=$(attach bracket "$FIXTURES/ctc-01/step/nist_ctc_01_asme1_rd.stp" mounting_bracket.stp)
    attach bracket "$FIXTURES/ctc-01/inventor-2021/INV_nist_ctc_01_asme1_2021.ipt" >/dev/null
    attach bracket "$FIXTURES/ctc-01/solidworks-2018/nist_ctc_01_asme1_rd_sw1802.SLDPRT" >/dev/null
    attach bracket "$FIXTURES/ctc-01/nx-1980/nist_ctc_01_asme1_nx1980_rd.prt" >/dev/null
    printf 'Mounting bracket - inspection notes\n\nFlatness of the mounting face: 0.05 mm.\n' >"$WORK/inspection-notes.txt"
    attach bracket "$WORK/inspection-notes.txt" inspection-notes.txt >/dev/null
    printf 'Mounting bracket - inspection notes\n\nFlatness of the mounting face: 0.05 mm.\nAnodise after machining (type III, black).\n' >"$WORK/inspection-notes.txt"
    attach bracket "$WORK/inspection-notes.txt" inspection-notes.txt "Anodising added" >/dev/null
    attach clamp "$FIXTURES/ftc-11/step/nist_ftc_11_asme1_rb.stp" cable_clamp.stp >/dev/null
    attach clamp "$FIXTURES/ftc-11/inventor-2021/INV_nist_ftc_11_asme1_2021.ipt" >/dev/null
    attach clamp "$FIXTURES/ftc-11/solidworks-2018/nist_ftc_11_asme1_rb_sw1802.SLDPRT" >/dev/null

    log "building the multi-level BOM"
    bom_line mast enclosure 1 10
    bom_line mast bracket 2 20 "Left and right"
    bom_line mast clamp 4 30
    bom_line mast shcs_l 8 40 "Bracket to mast tube"
    bom_line mast nut 8 50
    bom_line enclosure box 1 10
    bom_line enclosure cover 1 20
    bom_line enclosure plate 1 30
    bom_line enclosure window 1 40
    bom_line enclosure shcs_s 12 50 "Cover screws, 3 Nm"
    bom_line enclosure fhcs 4 60

    log "checking everything in"
    local key
    for key in mast enclosure box cover plate window shcs_s shcs_l fhcs nut bracket clamp; do
        checkin "$key"
    done

    log "releasing the fasteners through a reviewed change order"
    local eco1 eco2
    eco1=$(japi mchen POST /changes \
        '{"title":"Release of the standard fasteners","description":"Purchased parts from the catalogue, no drawing needed.","priority":"normal"}' |
        jq -r '.number')
    for key in shcs_s shcs_l fhcs nut; do
        japi mchen POST "/changes/$eco1/items" "$(jq -nc --arg i "${NUM[$key]}" '{item: $i}')" >/dev/null
    done
    japi mchen POST "/changes/$eco1/reviewers" '{"username":"admin"}' >/dev/null
    japi mchen POST "/changes/$eco1/reviewers" '{"username":"jlopez"}' >/dev/null
    api mchen POST "/changes/$eco1/submit" >/dev/null
    japi admin POST "/changes/$eco1/review" '{"decision":"approve","comment":"Catalogue data checked."}' >/dev/null
    japi jlopez POST "/changes/$eco1/review" '{"decision":"approve"}' >/dev/null

    log "putting the enclosure change into review"
    local out
    out=$(japi mchen POST /changes \
        '{"title":"Thicker enclosure cover for the IP67 seal","description":"The cover flexes under the gasket load. Wall thickness 3 mm to 4 mm, screw pattern unchanged.","priority":"high"}')
    eco2=$(jq -r '.number' <<<"$out")
    CHANGE_ID=$(jq -r '.id' <<<"$out")
    for key in cover enclosure; do
        japi mchen POST "/changes/$eco2/items" "$(jq -nc --arg i "${NUM[$key]}" '{item: $i}')" >/dev/null
    done
    japi mchen POST "/changes/$eco2/reviewers" '{"username":"admin"}' >/dev/null
    japi mchen POST "/changes/$eco2/reviewers" '{"username":"jlopez"}' >/dev/null
    api mchen POST "/changes/$eco2/submit" >/dev/null
    japi jlopez POST "/changes/$eco2/review" \
        '{"decision":"approve","comment":"Gasket compression is fine with 4 mm, checked in the FE model."}' >/dev/null

    log "work in progress: check-outs and watches"
    checkout bracket admin
    checkout clamp jlopez
    api admin POST "/items/${NUM[mast]}/watch" >/dev/null
    TOP_REV=${REV[mast]}
    ENCLOSURE_REV=${REV[enclosure]}
    BRACKET_REV=${REV[bracket]}
    log "demo data ready (change orders $eco1 and $eco2)"
}

# ---------------------------------------------------------------------------
# WebDriver (W3C) through curl
# ---------------------------------------------------------------------------

# wd METHOD PATH [JSON] – prints the response; stops on a WebDriver error. The
# body goes through stdin so that typed passwords never show up in argv.
wd() {
    local method=$1 path=$2 body=${3-} out
    if [[ -n $body ]]; then
        out=$(printf '%s' "$body" | curl -sS --max-time 180 -X "$method" \
            -H 'Content-Type: application/json' --data-binary @- "$WD$path") ||
            die "WebDriver $method $path: no answer"
    else
        out=$(curl -sS --max-time 180 -X "$method" "$WD$path") ||
            die "WebDriver $method $path: no answer"
    fi
    if [[ $(jq -r '(.value | objects | .error) // empty' <<<"$out" 2>/dev/null) != "" ]]; then
        die "WebDriver $method $path: $(jq -r '.value.error + ": " + ((.value.message // "") | split("\n")[0])' <<<"$out")"
    fi
    printf '%s' "$out"
}

# js SCRIPT [ARGS-JSON] – runs a synchronous script in the page, prints .value.
js() {
    wd POST "/session/$SESSION/execute/sync" \
        "$(jq -nc --arg s "$1" --argjson a "${2:-[]}" '{script: $s, args: $a}')" | jq -r '.value'
}

cdp() {
    wd POST "/session/$SESSION/goog/cdp/execute" \
        "$(jq -nc --arg c "$1" --argjson p "$2" '{cmd: $c, params: $p}')" >/dev/null
}

start_browser() {
    local driver=${CHROMEDRIVER:-}
    if [[ -z $driver ]]; then
        driver=$(command -v chromedriver || true)
    fi
    if [[ -z $driver && -n ${CHROMEWEBDRIVER:-} ]]; then
        driver=$CHROMEWEBDRIVER/chromedriver
    fi
    [[ -n $driver && -x $driver ]] || die "chromedriver not found (set CHROMEDRIVER)"
    log "starting $driver"
    "$driver" --port="$WD_PORT" >"$WORK/chromedriver.log" 2>&1 &
    DRIVER_PID=$!
    local i
    for ((i = 0; i < 30; i++)); do
        if curl -s --max-time 2 "$WD/status" 2>/dev/null | jq -e '.value.ready == true' >/dev/null 2>&1; then
            break
        fi
        kill -0 "$DRIVER_PID" 2>/dev/null || die "chromedriver exited"
        sleep 1
    done
    ((i < 30)) || die "chromedriver did not become ready"

    local caps
    caps=$(jq -nc --arg bin "${CHROME_BIN:-}" '{
        capabilities: {alwaysMatch: {
            browserName: "chrome",
            pageLoadStrategy: "normal",
            timeouts: {pageLoad: 120000, script: 60000, implicit: 0},
            "goog:chromeOptions": ({
                args: ["--headless=new", "--no-sandbox", "--disable-dev-shm-usage",
                       "--window-size=1440,900", "--hide-scrollbars", "--lang=en-US",
                       "--force-color-profile=srgb", "--font-render-hinting=none",
                       "--use-angle=swiftshader", "--enable-unsafe-swiftshader",
                       "--disable-features=Translate,MediaRouter", "--no-first-run"]
            } + (if $bin == "" then {} else {binary: $bin} end))
        }}}')
    SESSION=$(wd POST /session "$caps" | jq -r '.value.sessionId')
    [[ -n $SESSION && $SESSION != null ]] || die "no WebDriver session"
    log "browser session started"
}

# viewport WIDTH HEIGHT SCALE MOBILE – exact CSS viewport through DevTools
# emulation (the headless window size would include no browser chrome anyway).
viewport() {
    cdp Emulation.setDeviceMetricsOverride \
        "$(jq -nc --argjson w "$1" --argjson h "$2" --argjson s "$3" --argjson m "$4" \
            '{width: $w, height: $h, deviceScaleFactor: $s, mobile: $m}')"
    cdp Emulation.setEmulatedMedia '{"features":[{"name":"prefers-color-scheme","value":"light"}]}'
}

# wait_js EXPRESSION SECONDS WHAT – polls a JS expression until it is true.
# Errors while a page is navigating are expected and retried.
wait_js() {
    local cond=$1 limit=$2 what=$3 start=$SECONDS body value
    body=$(jq -nc --arg s "return Boolean($cond);" '{script: $s, args: []}')
    while true; do
        value=$(printf '%s' "$body" | curl -s --max-time 30 -X POST \
            -H 'Content-Type: application/json' --data-binary @- \
            "$WD/session/$SESSION/execute/sync" 2>/dev/null | jq -r '.value' 2>/dev/null || true)
        [[ $value == true ]] && return 0
        ((SECONDS - start < limit)) || die "timed out after ${limit}s waiting for $what"
        sleep 1
    done
}

open_page() {
    wd POST "/session/$SESSION/url" "$(jq -nc --arg u "$BASE_URL$1" '{url: $u}')" >/dev/null
    wait_js 'document.readyState === "complete"' 60 "the page $1 to load"
    if [[ $1 != /login && $(js 'return location.pathname;') == /login ]]; then
        die "$1 redirected to the login page"
    fi
    wait_js '!document.fonts || document.fonts.status === "loaded"' 30 "the web fonts"
    wait_js 'Array.from(document.images).every(function (i) { return i.complete; })' 60 "the images on $1"
}

element() {
    wd POST "/session/$SESSION/element" "$(jq -nc --arg s "$1" '{using: "css selector", value: $s}')" |
        jq -r '.value["element-6066-11e4-a52e-4f735466cecf"]'
}

sign_in_browser() {
    log "signing in through the login form"
    open_page /login
    local user pass button
    user=$(element 'form[action="/login"] input[name="username"]')
    pass=$(element 'form[action="/login"] input[name="password"]')
    button=$(element 'form[action="/login"] button[type="submit"]')
    wd POST "/session/$SESSION/element/$user/value" '{"text":"admin"}' >/dev/null
    wd POST "/session/$SESSION/element/$pass/value" \
        "$(jq -nc --rawfile p "$WORK/pw-admin" '{text: $p}')" >/dev/null
    wd POST "/session/$SESSION/element/$button/click" '{}' >/dev/null
    wait_js 'location.pathname !== "/login" && document.readyState === "complete"' 30 "the sign-in"
}

# shot NAME PATH [SCROLL-SELECTOR] – opens PATH and saves $OUT_DIR/NAME.png.
shot() {
    local name=$1 path=$2 scroll=${3:-}
    open_page "$path"
    if [[ -n $scroll ]]; then
        [[ $(js 'var el = document.querySelector(arguments[0]);
            if (el) { window.scrollTo(0, el.getBoundingClientRect().top + window.scrollY - 24); }
            return Boolean(el);' "$(jq -nc --arg s "$scroll" '[$s]')") == true ]] ||
            die "$path has no element '$scroll'"
    fi
    snap "$name"
}

# snap NAME – saves the current viewport as $OUT_DIR/NAME.png.
snap() {
    local name=$1 file
    file="$OUT_DIR/$name.png"
    # Let transitions, lazy images and the WebGL viewer settle.
    sleep 1.5
    wd GET "/session/$SESSION/screenshot" | jq -r '.value' | base64 -d >"$file" ||
        die "could not decode the screenshot $name"
    if [[ $(head -c 8 "$file" | base64) != iVBORw0KGgo* ]] || [[ $(wc -c <"$file") -lt 10000 ]]; then
        die "$file is not a plausible PNG"
    fi
    log "  $file ($(($(wc -c <"$file") / 1024)) KiB)"
}

capture() {
    mkdir -p "$OUT_DIR"
    start_browser
    viewport 1440 900 "$SCALE" false
    sign_in_browser

    log "capturing desktop screenshots (1440x900)"
    shot dashboard /
    shot vault-explorer "/vault?folder=$PROJECT_ID&rev=$ENCLOSURE_REV"
    shot revision "/revisions/$BRACKET_REV"
    shot revision-files "/revisions/$BRACKET_REV" '#files'

    log "waiting for the 3D preview of the STEP file"
    open_page "/files/$STEP_FILE/preview"
    wait_js 'document.getElementById("viewer-canvas").getAttribute("data-ready") !== null' 240 \
        "the 3D preview (STEP conversion)"
    if [[ $(js 'return document.getElementById("viewer-canvas").getAttribute("data-ready");') != true ]]; then
        die "the 3D viewer reported: $(js 'var s = document.getElementById("viewer-status"); return s ? s.textContent : "";')"
    fi
    snap viewer-3d
    shot bom "/revisions/$TOP_REV/bom"
    shot change-review "/changes/$CHANGE_ID"
    shot notifications /notifications

    log "capturing the mobile screenshot (390x844)"
    viewport 390 844 2 true
    shot mobile-dashboard /
}

main() {
    wait_for_server
    seed
    capture
    log "done: $(find "$OUT_DIR" -maxdepth 1 -name '*.png' | wc -l) screenshots in $OUT_DIR"
}

main "$@"

#!/usr/bin/env python3
"""Read client mock outputs through native WP storage in two owned sites."""

import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parents[4]
spec = importlib.util.spec_from_file_location(
    "owned_wp", ROOT / "tests/infra/tools/owned-wp-sites.py"
)
owned_wp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(owned_wp)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--context", type=Path, required=True)
    parser.add_argument("--outputs", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    context = owned_wp.load(args.context)
    if context["plugin"] != "wpmmcc-ats":
        raise ValueError("requires the owned ATS fixture")
    source, untouched = context["sites"]
    for site in context["sites"]:
        label = subprocess.check_output([
            "docker", "inspect", site["name"], "--format",
            '{{index .Config.Labels "com.wptsall.owned.run"}}',
        ], text=True).strip()
        if label != context["owner"]:
            raise ValueError("owned WP label mismatch")
    outputs = {
        key: (args.outputs / key).read_text()
        for key in ("translated.phpser", "translated.json", "translated.html")
    }
    title = "sol61-structured-" + context["owner"]
    php = f"""
function require_true($ok, $message) {{ if (!$ok) throw new RuntimeException($message); }}
$title={json.dumps(title)};
$serialized={json.dumps(outputs['translated.phpser'], ensure_ascii=False)};
$config=unserialize($serialized, array('allowed_classes'=>false));
require_true(is_array($config) && $config['id']==='node-1', 'serialized shape');
$json={json.dumps(outputs['translated.json'], ensure_ascii=False)};
$data=json_decode($json, true);
require_true(is_array($data) && $data['widgetType']==='heading', 'JSON shape');
$html={json.dumps(outputs['translated.html'], ensure_ascii=False)};
$id=wp_insert_post(array('post_title'=>$title,'post_status'=>'publish',
 'post_content'=>wp_slash($html)), true);
require_true(!is_wp_error($id) && $id>0, 'native post insert');
update_post_meta($id, 'sol61_structured_config', $config);
update_post_meta($id, 'sol61_json_config', wp_slash($json));
update_option('sol61_structured_config', $config);
clean_post_cache($id);
require_true(get_post_meta($id,'sol61_structured_config',true)===$config, 'native meta read');
require_true(get_option('sol61_structured_config')===$config, 'native option read');
require_true(json_decode(get_post_meta($id,'sol61_json_config',true),true)===$data, 'native JSON read');
require_true(get_post($id)->post_content===$html, 'native HTML read');
require_true(strpos($html,'【zh】Hero')!==false, 'nonempty changed output');
echo json_encode(array('post_id'=>$id,'title'=>$title));
"""
    inserted = json.loads(owned_wp.wp(source["name"], php))
    reloaded = json.loads(owned_wp.wp(source["name"], f"""
$id={inserted['post_id']}; clean_post_cache($id);
$meta=get_post_meta($id,'sol61_structured_config',true);
$json=json_decode(get_post_meta($id,'sol61_json_config',true),true);
$post=get_post($id);
if(!is_array($meta) || $meta['id']!=='node-1' || $meta['title']!=='【zh】Hero【/zh】'
 || $json['id']!=='node-1' || strpos($post->post_content,'<!-- wp:group {{"id":7}} -->')===false)
 throw new RuntimeException('independent WP reload failed');
echo json_encode(array('meta_shape'=>true,'json_shape'=>true,'block_marker'=>true));
"""))
    if not all(reloaded.values()):
        raise ValueError("native reload checks failed")
    other = owned_wp.wp(untouched["name"], f"""
$posts=get_posts(array('post_type'=>'post','post_status'=>'any','s'=>{json.dumps(title)}));
if(count($posts)!==0 || get_option('sol61_structured_config',null)!==null)
 throw new RuntimeException('second owned site was written');
echo 'untouched';
""")
    if other != "untouched":
        raise ValueError("second owned site check missing")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    url = source["base_url"] + "/?p=" + str(inserted["post_id"])
    for _ in range(2):
        with opener.open(url, timeout=10) as response:
            body = response.read().decode()
        if "【zh】Hero" not in body or "Hello {name}" not in body:
            raise ValueError("front-end independent reload has no translated output")
    report = {
        "kind": "owned-wp-native-structured-consumer",
        "plugin": "wpmmcc-ats",
        "native_php_meta_and_option_roundtrip": True,
        "native_json_meta_roundtrip": True,
        "block_content_roundtrip": True,
        "independent_native_read": True,
        "front_end_two_requests": True,
        "second_owned_site_untouched": True,
        "post_id": inserted["post_id"],
        "production_client_callback_verified": False,
        "browser_verified": False,
    }
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print("Owned WP native structured consumer PASS; not a callback/GUI claim")


if __name__ == "__main__":
    main()

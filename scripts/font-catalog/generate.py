#!/usr/bin/env python3
"""Pin Classic's static Google faces without committing downloaded font binaries.

Network regeneration is explicit; urllib preserves configured proxy variables.
The cache lives outside the checkout. Each source is fixed to COMMIT; failures
are recorded as unavailable rather than creating a fallback or fabricated hash.
"""
import argparse, concurrent.futures, hashlib, json, re, struct, time, urllib.request
from pathlib import Path
COMMIT = '66928797f271fed5ee428544e7229685e2d90127'
ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'crates/editor-application/src/font_import_v1'
BASE = f'https://raw.githubusercontent.com/google/fonts/{COMMIT}/'
STYLE = {'Regular.ttf': (400,'normal'), 'Bold.ttf': (700,'normal'), 'Italic.ttf': (400,'italic'), 'BoldItalic.ttf': (700,'italic')}
def digest(data): return hashlib.sha256(data).hexdigest()
def norm(s): return re.sub('[^a-z0-9]','',s.lower())
def get(path, cache):
    target=cache / path
    if target.exists(): return target.read_bytes()
    for attempt in range(3):
        try:
            with urllib.request.urlopen(BASE + urllib.parse.quote(path),timeout=35) as r:
                data=r.read(32*1024*1024+1)
            if len(data)>32*1024*1024: raise ValueError('source exceeds font envelope')
            target.parent.mkdir(parents=True, exist_ok=True); target.write_bytes(data); return data
        except Exception:
            if attempt==2: raise
            time.sleep(attempt+1)
def face(data):
    """Read declared sfnt identity; runtime still independently parses all tables."""
    if data[:4] not in (b'\x00\x01\x00\x00',b'OTTO'): raise ValueError('unsupported sfnt')
    count=struct.unpack_from('>H',data,4)[0]
    if not 1<=count<=128: raise ValueError('native table-count envelope exceeded')
    tables={};ranges=[];prior=None
    for i in range(count):
        tag,checksum,offset,size=struct.unpack_from('>4sIII',data,12+16*i)
        if prior is not None and tag<=prior:raise ValueError('native requires uniquely sorted tables')
        prior=tag
        if tag in (b'fvar',b'gvar',b'CFF2',b'avar',b'HVAR',b'VVAR',b'MVAR'):raise ValueError('native unsupported variable source')
        if tag in (b'COLR',b'SVG ',b'CBDT',b'sbix',b'EBDT',b'bdat'):raise ValueError('native unsupported color or bitmap tables')
        if offset<12+16*count or offset%4 or offset+size>len(data):raise ValueError('native invalid table range')
        if size:ranges.append((offset,offset+size))
        table=data[offset:offset+size];padded=table+b'\0'*((-len(table))%4)
        if tag==b'head':padded=padded[:8]+b'\0'*4+padded[12:]
        actual=sum(struct.unpack('>'+str(len(padded)//4)+'I',padded)) & 0xffffffff
        if actual!=checksum:raise ValueError('native table checksum mismatch')
        tables[tag]=table
    ranges.sort()
    if any(a[1]>b[0] for a,b in zip(ranges,ranges[1:])):raise ValueError('native overlapping font tables')
    if not {b'OS/2',b'name',b'cmap',b'hmtx'}.issubset(tables) or not (b'glyf' in tables or b'CFF ' in tables):raise ValueError('native required metadata/outlines missing')
    os2=tables[b'OS/2']; version,weight=struct.unpack_from('>H',os2,0)[0],struct.unpack_from('>H',os2,4)[0];flags=struct.unpack_from('>H',os2,62)[0]
    if not flags&1 and version>=4 and flags&512:raise ValueError('native oblique source')
    italic_angle=struct.unpack_from('>i',tables[b'post'],4)[0] if b'post' in tables else 0
    style='italic' if flags&1 or italic_angle else 'normal'; name=tables[b'name']; count,strings=struct.unpack_from('>HH',name,2); names=[]
    for i in range(count):
        platform,encoding,language,ident,length,offset=struct.unpack_from('>HHHHHH',name,6+12*i)
        if ident in (1,16) and (platform==0 or platform==3 and encoding in (0,1,10)):
            value=name[strings+offset:strings+offset+length].decode('utf-16-be')
            names.append(((not(platform==3 and language==0x409),ident!=16,value),value))
    if not names: raise ValueError('no unicode family')
    return min(names)[1],weight,style

def static_jobs(atlas, paths):
    """Discover original static bytes; filenames never establish face identity.

    Google keeps some static faces under static/ and historical filenames do
    not consistently contain Regular/Italic suffixes. Admission remains based
    on the verified sfnt metadata, exact family and atlas availability.
    """
    bynorm = {norm(f): f for f in atlas}
    bynorm['mashanzheng'] = 'Ma Shan Zheng'
    jobs = []
    for path, entry in sorted(paths.items()):
        parts = path.split('/')
        if (len(parts) not in (3, 4) or parts[0] not in ('ofl', 'apache')
                or parts[1] not in bynorm or '[' in path
                or not parts[-1].endswith('.ttf')
                or len(parts) == 4 and parts[2] != 'static'
                or entry.get('type', 'blob') != 'blob'):
            continue
        # Avoid fetching clearly inapplicable named weights. Unknown historical
        # names still require metadata inspection, never suffix inference.
        suffix = parts[-1].removesuffix('.ttf').split('-')[-1]
        if re.fullmatch(r'(?:Thin|ExtraLight|Light|Medium|SemiBold|ExtraBold|Black)(?:Italic)?', suffix):
            continue
        license_path = '/'.join(parts[:2]) + ('/OFL.txt' if parts[0] == 'ofl' else '/LICENSE.txt')
        if license_path in paths:
            declared = STYLE.get(parts[-1].split('-')[-1]) if len(parts) == 3 else None
            if declared is not None:
                token = str(declared[0]) + ('i' if declared[1] == 'italic' else '')
                if bynorm[parts[1]] in atlas and token not in atlas[bynorm[parts[1]]]['s']:
                    continue
            jobs.append((bynorm[parts[1]], path, license_path, declared, entry['size']))
    return jobs

def admitted_style(declared, weight, style):
    actual = (weight, style)
    if declared is not None and actual != declared:
        raise ValueError('declared weight/style/size mismatch')
    return actual

def select_records(records):
    # Preserve the former root filename candidates and their former path order.
    # New metadata-discovered sources only fill absent admissible faces.
    ordered = sorted(records, key=lambda r: (r['family'], r['weight'], r['style'],
        0 if len(r['path'].split('/')) == 3 and STYLE.get(r['path'].split('/')[-1].split('-')[-1]) is not None else 1,
        r['path']))
    seen = set()
    unique = []
    for record in ordered:
        key = (record['family'], record['weight'], record['style'])
        if key not in seen:
            unique.append(record)
            seen.add(key)
    return unique

def main():
    ap=argparse.ArgumentParser(); ap.add_argument('--tree',type=Path,required=True);ap.add_argument('--cache',type=Path,required=True);ap.add_argument('--workers',type=int,default=16);args=ap.parse_args()
    if ROOT==args.cache or ROOT in args.cache.parents: raise SystemExit('cache must be outside checkout')
    atlas_path=ROOT/'tmp/opencut-classic/apps/web/public/fonts/font-atlas.json';atlas_bytes=atlas_path.read_bytes();atlas=json.loads(atlas_bytes)['fonts']; tree=json.loads(args.tree.read_text()); assert not tree.get('truncated')
    paths={x['path']:x for x in tree['tree']};jobs=static_jobs(atlas,paths)
    records=[];failures=[]
    def pin(job):
        family,path,lp,style,size=job;data=get(path,args.cache);license_data=get(lp,args.cache);actual,weight,sl=face(data)
        style=admitted_style(style,weight,sl)
        token=str(weight)+('i' if sl=='italic' else '')
        if weight not in (400,700) or family in atlas and token not in atlas[family]['s']:raise ValueError('face not expressed by Classic TextV4 selection')
        if len(data)!=size:raise ValueError('declared source size mismatch')
        git_blob=hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()
        if git_blob != paths[path]['sha']:raise ValueError('source does not match fixed git blob')
        license_blob=hashlib.sha1(b'blob '+str(len(license_data)).encode()+b'\0'+license_data).hexdigest()
        if license_blob != paths[lp]['sha']:raise ValueError('license does not match fixed git blob')
        if actual!=family:raise ValueError(f'family mismatch: {actual}')
        notice=license_data.decode('utf-8');identifier='OFL-1.1' if lp.endswith('OFL.txt') else 'Apache-2.0'
        attribution=re.split(r'\r?\n\r?\n', notice)[0].rstrip() if identifier=='OFL-1.1' else ''
        suffix={ (400,'normal'):'regular',(700,'normal'):'bold',(400,'italic'):'italic',(700,'italic'):'bold-italic'}[style]
        return dict(catalog_id=f'google-{norm(family)}-{suffix}-v1',family=family,weight=weight,style=sl,path=path,content_hash=digest(data),size_bytes=size,license_path=lp,license_sha256=digest(license_data),license_identifier=identifier,license_notice=notice,attribution=attribution)
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as ex:
        futures={ex.submit(pin,j):j for j in jobs}
        for index,f in enumerate(concurrent.futures.as_completed(futures),1):
            try:records.append(f.result())
            except Exception as e:failures.append(dict(family=futures[f][0],path=futures[f][1],reason=str(e)))
            if index%100==0:print(f'{index}/{len(jobs)} pinned={len(records)} unavailable={len(failures)}',flush=True)
    records=select_records(records)
    # Existing B1 identities remain exact (including the historical hyphenated ID).
    for r in records:
        if r['family']=='Ma Shan Zheng':r['catalog_id']='google-mashanzheng-regular-v1'
    license_dir=OUT/'licenses/catalog';license_dir.mkdir(parents=True,exist_ok=True)
    lines=['// Generated by scripts/font-catalog/generate.py; do not hand edit.','&[']
    for r in records:
        license_file=license_dir/(r['license_sha256']+'.txt');license_file.write_bytes(r['license_notice'].encode())
        lines.append('    PinnedFontSourceV1 {')
        for field in ('catalog_id','family','weight','style','source_commit','source_url','content_hash','size_bytes','license_url','license_sha256','license_identifier','license_notice','attribution'):
            if field=='style':value='FontStyleV4::'+('Normal' if r[field]=='normal' else 'Italic')
            elif field=='source_commit':value='GOOGLE_FONTS_COMMIT_V1'
            elif field=='source_url':value=json.dumps(BASE+urllib.parse.quote(r['path']))
            elif field=='license_url':value=json.dumps(BASE+urllib.parse.quote(r['license_path']))
            elif field=='license_notice':value=f'include_str!("licenses/catalog/{r["license_sha256"]}.txt")'
            elif isinstance(r[field],int):value=str(r[field])
            else:value=json.dumps(r[field],ensure_ascii=False)
            lines.append(f'        {field}: {value},')
        lines.append('    },')
    lines.append(']');(OUT/'catalog_generated.rs').write_text('\n'.join(lines)+'\n')
    byfamily={}
    for r in records:byfamily.setdefault(r['family'],{})[str(r['weight'])+('i' if r['style']=='italic' else '')]=r['catalog_id']
    browser=[]
    for family,entry in sorted(atlas.items()):
        styles=[]
        for token in entry['s']:
            weight=int(token.rstrip('i'));style='italic' if token.endswith('i') else 'normal';cid=byfamily.get(family,{}).get(token)
            reason=None if cid else ('weight_not_expressed_by_text_v4' if weight not in (400,700) else 'no_pinned_static_source')
            styles.append(dict(weight=weight,style=style,catalog_id=cid,unavailable_reason=reason))
        browser.append(dict(family=family,category='google',faces=styles))
    for family in ['Arial','Helvetica','Times New Roman','Courier New','Verdana','Georgia','monospace','sans-serif','serif']:
        browser.append(dict(family=family,category='system',faces=[dict(weight=w,style=s,catalog_id=None,unavailable_reason='system_font_not_redistributable') for w in (400,700) for s in ('normal','italic')]))
    manifest=dict(source_commit=COMMIT,classic_atlas_sha256=digest(atlas_bytes),classic_family_count=len(atlas),pinned_face_count=len(records),families=browser)
    (OUT/'browser_catalog.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n')
    report=dict(source_commit=COMMIT,attempted_static_faces=len(jobs),pinned_faces=len(records),pinned_families=len(byfamily),failures=sorted(failures,key=lambda x:x['path']))
    (OUT/'catalog_source_report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps({k:v for k,v in report.items() if k!='failures'}),flush=True)
if __name__=='__main__':main()

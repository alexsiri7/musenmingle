# London borough boundaries

`src/london_boroughs.geojson` holds the 33 London local authorities (32
boroughs and the City of London), used by `src/borough.rs` to find an
event's borough from its coordinates (issue #79).

- **Source:** Office for National Statistics, *Local Authority Districts
  (December 2024) Boundaries UK BFE* (full extent, so the Thames is split
  between the boroughs on either bank), features with codes `E09*`.
- **Licence:** Open Government Licence v3.0. Attribution (shown on
  `/about#boroughs`): "Source: Office for National Statistics licensed
  under the Open Government Licence v3.0. Contains OS data © Crown copyright
  and database right 2024."
- **Simplification:** Douglas-Peucker per ring at 0.0001° (about 7 m east-west,
  11 m north-south), coordinates rounded to 5 decimals, properties reduced
  to `code` and `name`, every geometry a `MultiPolygon`. About 300 KB. Rings
  are simplified independently, so neighbouring borders can leave slivers a
  few metres wide: a point there gets the first borough by key, or none.

Borough names must match `crate::borough::BOROUGHS` (a test checks all 33
load). Boundaries change rarely; when they do, regenerate the file, run
`cargo test borough`, and update the year in the attribution on `/about`.

## Regenerating

```bash
S="https://services1.arcgis.com/ESMARspQHYMw9BZ9/arcgis/rest/services/Local_Authority_Districts_December_2024_Boundaries_UK_BFE/FeatureServer/0"
curl -s "$S/query" --data-urlencode "where=LAD24CD LIKE 'E09%'" \
  -d "outFields=LAD24CD,LAD24NM&outSR=4326&f=geojson&geometryPrecision=5" -o raw.geojson
python3 simplify.py 0.0001   # writes out.geojson
cp out.geojson src/london_boroughs.geojson
```

`simplify.py`:

```python
import json,sys
eps=float(sys.argv[1])
def dp(pts):
    if len(pts)<3: return pts
    keep=[False]*len(pts); keep[0]=keep[-1]=True
    stack=[(0,len(pts)-1)]
    while stack:
        a,b=stack.pop()
        ax,ay=pts[a];bx,by=pts[b]
        dx,dy=bx-ax,by-ay; L=(dx*dx+dy*dy)**.5
        best=-1;bi=None
        for i in range(a+1,b):
            px,py=pts[i]
            d=abs(dy*(px-ax)-dx*(py-ay))/L if L else ((px-ax)**2+(py-ay)**2)**.5
            if d>best: best=d;bi=i
        if bi is not None and best>eps:
            keep[bi]=True; stack+= [(a,bi),(bi,b)]
    return [p for p,k in zip(pts,keep) if k]
d=json.load(open('raw.geojson'))
feats=[];n=0
for f in sorted(d['features'],key=lambda f:f['properties']['LAD24NM']):
    g=f['geometry']
    polys=[g['coordinates']] if g['type']=='Polygon' else g['coordinates']
    out=[]
    for poly in polys:
        rings=[]
        for r in poly:
            s=dp(r)
            if len(s)>=4: rings.append([[round(x,5),round(y,5)] for x,y in s]); n+=len(s)
        if rings: out.append(rings)
    feats.append({"type":"Feature","properties":{"code":f['properties']['LAD24CD'],"name":f['properties']['LAD24NM']},"geometry":{"type":"MultiPolygon","coordinates":out}})
s=json.dumps({"type":"FeatureCollection","features":feats},separators=(',',':'))
open('out.geojson','w').write(s); print(eps,len(s),n)
```

#!/usr/bin/env python3
"""Conservative source-publication hygiene scan; not a full secret scanner/audit."""
import pathlib, re, subprocess, json
root=pathlib.Path(__file__).resolve().parents[1]
paths=subprocess.check_output(['git','ls-files','--cached','--others','--exclude-standard'],cwd=root,text=True).splitlines()
patterns=[r'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----',r'gh[pousr]_[A-Za-z0-9]{30,}',r'AKIA[A-Z0-9]{16}']
hits=[]
for name in sorted(set(paths)):
 p=root/name
 if p.is_file():
  text=p.read_text(errors='replace')
  if any(re.search(pattern,text) for pattern in patterns): hits.append(name)
print('Source hygiene candidates:',len(hits))
for name in hits: print('REVIEW:',name) # Never print values.
assert not hits, 'public source scan requires review'
metadata=root/'target/dependency-inventory.json'
if metadata.exists():
 packages=json.loads(metadata.read_text())['packages']
 inventory=[{'name':p['name'],'version':p['version'],'license':p['license'],'source':p['source']} for p in packages]
 out=root/'docs/verification/dependency-licenses.json'
 out.parent.mkdir(parents=True,exist_ok=True)
 out.write_text(json.dumps(inventory,indent=2)+'\n')
 print('Dependency license metadata inventory:',len(inventory))

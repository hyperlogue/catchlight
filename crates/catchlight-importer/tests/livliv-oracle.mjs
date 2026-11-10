// Test-only oracle: execute the pinned upstream CPU rig, never our port.
// Run by the ignored Rust test with LIVLIV_PLAYER_DIR set. No model artwork
// is copied: the synthetic images below are opaque white. GL calls are no-ops;
// the upstream build/deform/render code still computes all vertex positions.
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
const { Puppet, readModel, defaultParams } = await import(pathToFileURL(`${process.env.LIVLIV_PLAYER_DIR}/src/runtime/puppet.ts`));
const input = JSON.parse(readFileSync(0, 'utf8'));
globalThis.document = { createElement() {
  return { width: 0, height: 0, getContext() {
    return { drawImage() {}, getImageData(_x,_y,w,h) { return {data:new Uint8ClampedArray(w*h*4).fill(255)}; } };
  } };
} };
globalThis.window = {devicePixelRatio:1};
const gl = new Proxy({}, {get:()=>()=>({})});
const canvas = {getContext:()=>gl,clientWidth:96,clientHeight:96,width:96,height:96};
const puppet = new Puppet(canvas);
const model = readModel(input.manifest);
const img = (l)=>({width:l.w,height:l.h,naturalWidth:l.w,naturalHeight:l.h});
const images = model.layers.map(l=>({main:img(l),frames:(l.frames??[]).map(()=>img(l)),mask:l.mask?img(l):undefined}));
puppet.build(model, images, input.keyforms, []);
puppet.shown = ()=>true;
puppet.drawLayer = (layer,_texture,_alpha,vertices)=>{ layer.sample=Array.from(vertices??layer.out); };
const result = input.poses.map(spec=>{
  const p = {...defaultParams(),...spec.params};
  puppet.params=p;
  puppet.body={yaw:p.bodyX+p.angleX*puppet.rig.body.yaw*puppet.tuning.turn,roll:p.bodyZ+p.angleZ*puppet.rig.body.roll*puppet.tuning.turn};
  puppet.mouthFrame=spec.frame??0;
  puppet.mouthW=puppet.frameKind.map((_,i)=>i===puppet.mouthFrame?1:0);
  puppet.layers.forEach(l=>{delete l.sample;});
  puppet.deform();
  puppet.render();
  return puppet.layers.map(l=>({rest:Array.from(l.rest),out:l.sample??Array.from(l.out)}));
});
process.stdout.write(JSON.stringify(result));

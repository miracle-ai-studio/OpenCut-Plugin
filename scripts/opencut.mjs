#!/usr/bin/env node
// Explicit source host assembly. Native commands still own all business semantics.
import path from 'node:path';
import {spawn} from 'node:child_process';
import {rendererEnvironment} from '../plugins/opencut/scripts/runtime.mjs';
try {
 const [option,binaryPath,...args]=process.argv.slice(2);
 if(option!=='--binary'||!binaryPath)throw Error('Usage: node scripts/opencut.mjs --binary /path/to/opencut [native CLI arguments]');
 const binary=path.resolve(binaryPath);
 const env=await rendererEnvironment(binary);
 const child=spawn(binary,args,{stdio:'inherit',env});
 for(const signal of ['SIGINT','SIGTERM'])process.on(signal,()=>child.kill(signal));
 child.on('error',error=>{console.error(error.message);process.exitCode=1;});
 child.on('exit',(code,signal)=>{process.exitCode=code??(signal?1:0);});
} catch(error){console.error(error.message);process.exitCode=1;}

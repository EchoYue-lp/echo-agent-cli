"""逐角色来源的小文件控制；所有材料均为synthetic，绝不执行其程序。"""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import shlex
import stat
import subprocess
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent

def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

RUNNER = load('role_runner', 'performance-baseline-runner.py')
REPORT = load('role_report', 'performance-baseline-report.py')

def ref(path):
    return {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'bytes': path.stat().st_size}

def write(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False) + '\n')
    return ref(path)

def git(root, *args):
    return subprocess.run(['git', '-C', str(root), *args], capture_output=True, check=True).stdout

def current_fixture(root):
    roots, formal, sources = {}, {}, {}
    for name in ('eko', 'framework'):
        original = root / ('formal-' + name)
        original.mkdir()
        git(original, 'init', '--quiet')
        (original / 'Cargo.toml').write_text('# synthetic ' + name + '\n')
        (original / 'source.rs').write_text('// synthetic UTF-8 中文🙂\n')
        (original / 'source.rs').chmod(0o755)
        units = ('echo-agent-app-core/src/lib.rs', 'echo-agent-app-core/tests/performance_baseline.rs', 'src/lib.rs') if name == 'eko' else ('nested/source.rs',)
        for unit in units:
            path=original/unit;path.parent.mkdir(parents=True,exist_ok=True);path.write_text('// synthetic compile unit '+unit+'\n')
        git(original, 'add', '.')
        git(original, '-c', 'commit.gpgsign=false', '-c', 'user.name=Control', '-c', 'user.email=control@example.invalid', 'commit', '--quiet', '-m', 'synthetic fixture only')
        target = root / 'source' / name
        target.mkdir(parents=True)
        rows = []
        for path in sorted(original.rglob('*')):
            if '.git' in path.relative_to(original).parts or not path.is_file():
                continue
            relative=path.relative_to(original);destination = target / relative;destination.parent.mkdir(parents=True,exist_ok=True)
            shutil.copy2(path, destination)
            data = destination.read_bytes()
            mode = stat.S_IMODE(path.stat().st_mode)
            if name == 'framework':
                destination.chmod(mode & ~0o222)
            rows.append({'path': str(relative), 'kind': 'regular', 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(),
                         'git_blob': hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest(),
                         'git_mode': '100755' if mode & 0o111 else '100644', 'actual_mode': oct(stat.S_IMODE(destination.stat().st_mode))})
        if name == 'framework':
            for directory in (path for path in target.rglob('*') if path.is_dir()):directory.chmod(0o555)
            target.chmod(0o555)
        roots[name], formal[name] = target, original
        sources[name] = {'commit': git(original, 'rev-parse', 'HEAD').decode().strip(), 'formal_root': str(original), 'actual_build_source': str(target),
                         'member_count': len(rows), 'relative_member_set_complete': True, 'Git_blob_bytes_verified': True,
                         'actual_readonly_root_and_children': name == 'framework', 'writable_entries': [] if name == 'framework' else None, 'files': rows}
    manifest = write(root / 'manifest.json', {'state': 'EXACT_080_F340_SOURCE_MATERIALIZED_NO_CARGO', 'sources': sources})
    guard = write(root / 'guard.json', {'state': 'PASS080_ORDER1_POST_SOURCEGUARD0', 'source_manifest': manifest,
                  'results': {name: {'commit': source['commit'], 'files': source['member_count'], 'full_bytes_Gitblob_mode_exec_unchanged': True,
                                    'formal_clean': True, 'writable_F_including_root': []} for name, source in sources.items()}})
    supplied = {name: {'kind': 'git_object_snapshot', 'proof_format': 'materialization-v1', 'repository': name,
                      'origin_commit': source['commit'], 'manifest': manifest, 'sourceguard': guard} for name, source in sources.items()}
    return {'root': root, 'roots': roots, 'formal': formal, 'sources': sources, 'manifest': manifest, 'guard': guard, 'supplied': supplied}

def source_bridge(f, testcase):
    root = f['root']
    source = f['sources']['framework']
    index = write(root / 'p1-index.json', {'root': str(f['formal']['framework']), 'HEAD': source['commit'],
                 'members': [{'path': row['path'], 'bytes': row['bytes'], 'sha256': row['sha256'], 'mode': int(row['git_mode'], 8) & 0o777} for row in source['files']]})
    argv = ['cargo', 'test', '-p', 'echo_agent', '--release', '--lib', '--test', 'performance_baseline', '--features', 'files', '--locked', '--no-run']
    environment = {'RUSTC': '/synthetic/rustc', 'DYLD_LIBRARY_PATH': '/synthetic/lib'}
    context = write(root / 'p1-context.json', {'cwd': str(f['formal']['framework']), 'argv': argv, 'env': environment, 'source_index_sha256': index['sha256']})
    # 独立fixture生成时记录事实；冻结后修改不调用扫描器重立基线。
    physical=[];content=[]
    for row in source['files']:
        a=(f['formal']['framework']/row['path']).stat();b=(f['roots']['framework']/row['path']).stat()
        physical.append([row['path'],a.st_dev,a.st_ino,a.st_mtime_ns,a.st_ctime_ns,b.st_dev,b.st_ino,b.st_mtime_ns,b.st_ctime_ns])
        content.append({key:row[key] for key in ('path','git_mode','git_blob','bytes','sha256')})
    prior=write(root/'prior-freeze.json',{'state':'READONLY_DEBUG_PHASE3_PER_ROLE_PROVENANCE_ROOT_CAUSE_CONFIRMED','source_manifest':f['manifest'],'P1_source_index':index,
        'framework_shared_source_diagnostic':{'formal_root':str(f['formal']['framework']),'snapshot_root':str(f['roots']['framework']),
            'formal_root_mode':oct(stat.S_IMODE(f['formal']['framework'].stat().st_mode)),'snapshot_root_mode':oct(stat.S_IMODE(f['roots']['framework'].stat().st_mode)),
            'physical_inode_time_binding_sha256':RUNNER.digest(RUNNER.encoded(physical)), 'content_and_Gitmode_identity_sha256':RUNNER.digest(RUNNER.encoded(content))}})
    bridge = {'kind': 'framework-p1-same-source-v1', 'qualified_source': f['supplied']['framework'], 'source_index': index, 'compile_context': context,'prior_freeze':prior}
    directories={'.'}
    for row in source['files']:directories.update(str(path) for path in Path(row['path']).parents)
    def physical(path):
        info=path.lstat();return {'path':str(path),'device':str(info.st_dev),'inode':str(info.st_ino),'mode':oct(stat.S_IMODE(info.st_mode)),'mtime_ns':str(info.st_mtime_ns),'ctime_ns':str(info.st_ctime_ns)}
    authority=root/'.echo-semantic/local/root-full-framework-window-20261029';authority.mkdir(parents=True)
    records=[];file_records=[]
    for relative in sorted(directories):records.append({'path':relative,'formal':physical(f['formal']['framework']/relative),'snapshot':physical(f['roots']['framework']/relative)})
    for row in source['files']:
        value={key:row[key] for key in ('path','git_mode','git_blob')}
        for name,folder in (('formal',f['formal']['framework']),('snapshot',f['roots']['framework'])):
            path=folder/row['path'];info=path.stat();value[name]={'bytes':info.st_size,'sha256':ref(path)['sha256'],'mode':oct(stat.S_IMODE(info.st_mode)),'device':str(info.st_dev),'inode':str(info.st_ino)}
        file_records.append(value)
    anchor=write(authority/'F340-dual-root-directory-freeze-20261101-r01.json',{'at':'synthetic independent safety point','state':'ROOT_INDEPENDENT_NEW_DIRECTORY_FREEZE_POINT_NOT_HISTORICAL_COMPILE_METADATA',
        'formal_root':str(f['formal']['framework']),'snapshot_root':str(f['roots']['framework']),'commit':source['commit'],'git_tree':git(f['formal']['framework'],'rev-parse',source['commit']+'^{tree}').decode().strip(),
        'file_count':source['member_count'],'directory_count':len(directories),'expected_directory_modes':{'formal':'0o755','snapshot':'0o555'},'directories':records,'files':file_records})
    post=write(authority/'F340-dual-root-directory-freeze-postscan-20261101-r01.json',{'state':'ROOT_NEW_FREEZE_DIRECTORY_SECOND_SCAN_EXACT0','anchor':{key:anchor[key] for key in ('path','sha256')},
        'actual_roots':{'formal':str(f['formal']['framework']),'snapshot':str(f['roots']['framework'])},'drift':[],
        'independently_checked_directory_count_per_root':len(directories),'device_inode_kind_fullmode_mtime_ctime_all_equal_anchor':True})
    bridge.update(directory_anchor=anchor,directory_postscan=post)
    # 只替换单元测试的信任边界常量；生产代码没有覆盖锚SHA的参数/环境开关。
    for key,value in (('DIRECTORY_ANCHOR_SHA256',anchor['sha256']),('DIRECTORY_POSTSCAN_SHA256',post['sha256'])):
        patch=mock.patch.object(RUNNER,key,value);patch.start();testcase.addCleanup(patch.stop)
    bridge['frozen_state'] = RUNNER.scan_same_source_bridge(bridge)
    return bridge


def compile_fixture(f, role='core_default_lib'):
    root = f['root']
    values = {
        'core_default_lib': ('echo-agent-app-core', 'echo_agent_app_core', 'test-lib-echo_agent_app_core.json', [], True, []),
        'core_integration': ('echo-agent-app-core', 'performance_baseline', 'test-integration-test-performance_baseline.json', [], True, []),
        'core_testutils_lib': ('echo-agent-app-core', 'echo_agent_app_core', 'test-lib-echo_agent_app_core.json', ['test-utils'], True, ['test-utils']),
        'gui_lib': ('echo-agent-cli', 'echo_agent_cli', 'test-lib-echo_agent_cli.json', ['gui'], False, ['channels', 'gui'])}
    package, crate, fp_name, explicit, default, resolved = values[role]
    argv = ['cargo', 'test', '-p', package, '--release', '--lib', '--locked', '--no-run']
    if not default:
        argv.append('--no-default-features')
    if explicit:
        argv.extend(['--features', ','.join(explicit)])
    environment = {'RUSTC': '/synthetic/rustc', 'DYLD_LIBRARY_PATH': '/synthetic/lib', 'CARGO_INCREMENTAL': '0'}
    target=root/'synthetic-target';out_dir=target/'release/deps';environment['CARGO_TARGET_DIR']=str(target)
    suffix='-'+hashlib.sha256(role.encode()).hexdigest()[:16];stem=crate+suffix
    unit='src/lib.rs' if role=='gui_lib' else ('echo-agent-app-core/tests/performance_baseline.rs' if role=='core_integration' else 'echo-agent-app-core/src/lib.rs')
    description='tests/performance_baseline.rs' if role=='core_integration' else 'unittests src/lib.rs'
    raw = root / 'compile.raw';raw.write_text('  Executable '+description+' ('+str(out_dir/stem)+')\n')
    step = {'label': 'synthetic-current-NoRun', 'command': argv, 'cwd': str(f['roots']['eko']), 'logPath': str(raw), 'exitCode': 0,
            'signal': None, 'errorCode': None, 'truncated': False, 'bytesSeen': raw.stat().st_size, 'bytesWritten': raw.stat().st_size,
            'startedAt': '2026-11-01T00:00:00Z', 'finishedAt': '2026-11-01T00:00:01Z'}
    receipt = write(root / 'caller.json', {'schemaVersion': 1, 'steps': [step]})
    start = write(root / 'start.json', {'command': argv, 'cwd': step['cwd'], 'source_manifest': f['manifest'], 'exact_environment_overlay': environment})
    binary = root / 'retained-program';binary.write_text('synthetic program bytes; never executed\n');binary.chmod(0o755)
    profile = 7169924071199656256 if role == 'gui_lib' else 10034064700113017540
    fp = write(root / fp_name, {'profile': profile, 'features': json.dumps(resolved), 'local':[{'CheckDepInfo':{'dep_info':'release/.fingerprint/'+package+suffix+'/dep-'+fp_name.removesuffix('.json')}}]})
    dep = root / (stem+'.d');dep.write_text(str(out_dir/(stem+'.d'))+': '+unit+'\n')
    executable={'path':str(out_dir/stem),'description':'tests/performance_baseline.rs' if role=='core_integration' else 'unittests src/lib.rs'}
    terminal = write(root / 'terminal.json', {'receipt': step, 'warning_count': 0, 'error_count': 0, 'monitor_error': None, 'raw_sha256': ref(raw)['sha256'],'artifacts':[executable]})
    observed=[environment['RUSTC'],'--crate-name',crate,'--edition=2024',unit,'--test','--out-dir',str(out_dir),'-C','opt-level=3','-C','strip=debuginfo','-C','extra-filename='+suffix]
    for feature in resolved:observed+=['--cfg','feature='+json.dumps(feature)]
    artifact_role = {'role': role, 'retained_binary': {**ref(binary),'mode':'0o755'}, 'source_commit': f['sources']['eko']['commit'], 'source_manifest': f['manifest'],
                     'original_caller': receipt, 'post_sourceguard': {**f['guard'], 'mode': '0o644'}, 'loader_environment': {'DYLD_LIBRARY_PATH': environment['DYLD_LIBRARY_PATH']},
                     'actual_retained_FP': fp, 'actual_dep_info': ref(dep), 'actual_profile_u64_decimal': str(profile), 'actual_features_string': json.dumps(resolved),
                     'actual_final_rustc': [{'args':shlex.join(observed)}], 'original_Executable':executable,
                     'actual_emitted_binary':{**ref(binary),'mode':'0o755','path':executable['path']}}
    artifacts = write(root / 'artifacts.json', {'NoRun0_W0_postsourceguard0': True, 'actual_NoRun_caller': receipt, 'roles': [artifact_role],'target':str(target)})
    features = {'eko': {'default_features': default, 'all_features': False, 'explicit': explicit},
                'framework': {'default_features': True, 'all_features': False, 'explicit': ['files']}}
    contexts = {str(binary): {'kind': 'materialization-role-v1', 'role': role, 'artifacts': artifacts, 'compile_start': start, 'compile_terminal': terminal}}
    final = {'receipt': receipt, 'label': step['label'], 'command': argv, 'cwd': step['cwd'], 'repository': 'eko', 'profile': 'release', 'features': features,
             'environment_overlay': environment, 'retained_binaries': {str(binary): ref(binary)['sha256']}, 'role_provenance': contexts}
    profiles = {'profile': 'release', 'features': features, 'retained_binaries': final['retained_binaries'], 'source_inputs': f['supplied'],
                'evidence_reader': ref(HERE / 'performance-baseline-report.py'), 'source_identity_reader': ref(HERE / 'performance-baseline-runner.py')}
    return {'final': final, 'descriptor': {**receipt, 'final_steps': [final]}, 'profiles': profiles, 'binary': binary, 'artifacts': artifacts, 'role': artifact_role}


def qualify(f, compilation):
    return REPORT.compile_qualification(f['root'] / 'capsule-not-produced.json', compilation['descriptor'], set(), compilation['profiles'],
        {name: str(path) for name, path in f['roots'].items()}, compilation['binary'], ref(compilation['binary'])['sha256'])

class CurrentMaterialControls(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.f = current_fixture(Path(self.temporary.name))

    def test_current_public_source_identity_accepts_real_shape_without_faking_39c_state(self):
        for name, path in self.f['roots'].items():
            value = RUNNER.source_identity(path, self.f['supplied'][name])
            self.assertIsNone(value['head'])
            self.assertEqual(value['origin_commit'], self.f['sources'][name]['commit'])
            self.assertEqual(len(value['files']),self.f['sources'][name]['member_count'])
            self.assertEqual(json.loads(Path(self.f['guard']['path']).read_bytes())['state'], 'PASS080_ORDER1_POST_SOURCEGUARD0')

    def test_mixed_proof_or_unknown_format_never_falls_back(self):
        for change in ('mixed', 'format'):
            supplied = copy.deepcopy(self.f['supplied']['eko'])
            if change == 'mixed':
                supplied['git_objects'] = self.f['manifest']
            else:
                supplied['proof_format'] = 'try-anything'
            with self.assertRaises((ValueError, KeyError, OSError)):
                RUNNER.source_identity(self.f['roots']['eko'], supplied)

    def test_current_full_set_mode_and_guard_failures_do_not_fallback(self):
        for change in ('extra', 'mode', 'origin', 'guard'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                f = current_fixture(Path(directory));source = f['supplied']['eko']
                if change == 'extra': (f['roots']['eko'] / 'extra').write_text('extra')
                elif change == 'mode': (f['roots']['eko'] / 'source.rs').chmod(0o644)
                elif change == 'origin': source['origin_commit'] = 'f' * 40
                else:
                    path = Path(source['sourceguard']['path']);v=json.loads(path.read_bytes());v['state']='PASS_AFTER39C_POST_NORUN_SOURCEGUARD0';source['sourceguard']=write(path,v)
                with self.assertRaises(ValueError): RUNNER.source_identity(f['roots']['eko'], source)

    def test_real_dual_root_scanner_keeps_fullmodes_separate_and_freeze_dynamic(self):
        bridge = source_bridge(self.f,self)
        result = RUNNER.verify_same_source_bridge(bridge, self.f['formal']['framework'], self.f['roots']['framework'])
        self.assertEqual(result['member_count'],self.f['sources']['framework']['member_count'])
        self.assertTrue(result['actual_fullmodes_compared_separately'])
        original = self.f['formal']['framework'] / 'Cargo.toml'
        original.chmod(0o444)
        with self.assertRaisesRegex(ValueError, 'fullmode'): RUNNER.verify_same_source_bridge(bridge, self.f['formal']['framework'], self.f['roots']['framework'])
        original.chmod(0o644)
        # 同commit/同bytes/同mode，原物理文件已替换也不能借冻结证明通过。
        replacement = original.with_suffix('.new');replacement.write_bytes(original.read_bytes());replacement.chmod(0o644);replacement.replace(original)
        with self.assertRaisesRegex(ValueError, 'freeze|directory anchor'): RUNNER.verify_same_source_bridge(bridge, self.f['formal']['framework'], self.f['roots']['framework'])

    def test_bridge_rejects_root_alias_missing_freeze_and_formal_byte_drift(self):
        bridge = source_bridge(self.f,self)
        with self.assertRaises(ValueError): RUNNER.verify_same_source_bridge(bridge, self.f['roots']['framework'], self.f['roots']['framework'])
        missing = copy.deepcopy(bridge);missing.pop('frozen_state')
        with self.assertRaises(ValueError): RUNNER.verify_same_source_bridge(missing, self.f['formal']['framework'], self.f['roots']['framework'])
        (self.f['formal']['framework'] / 'source.rs').write_text('wrong byte source, same HEAD')
        with self.assertRaises(ValueError): RUNNER.verify_same_source_bridge(bridge, self.f['formal']['framework'], self.f['roots']['framework'])

    def test_public_compile_qualification_accepts_each_real_role_shape(self):
        for role in ('core_default_lib', 'core_integration', 'core_testutils_lib', 'gui_lib'):
            with self.subTest(role=role), tempfile.TemporaryDirectory() as directory:
                f = current_fixture(Path(directory));compilation=compile_fixture(f,role)
                result = qualify(f, compilation)
                self.assertEqual(result['status'], 'recognized_success')
                actual = result['steps'][0]['role_provenance'][0]
                self.assertEqual(actual['role'],role)
                self.assertEqual(actual['actual_profile_u64_decimal'],compilation['role']['actual_profile_u64_decimal'])

    def test_wrong_role_caller_env_default_for_GUI_and_ambiguous_program_are_rejected(self):
        for change in ('role', 'env', 'selection', 'ambiguous', 'FP', 'missing-provenance'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                f=current_fixture(Path(directory));c=compile_fixture(f,'gui_lib');p=str(c['binary'])
                if change=='role': c['final']['role_provenance'][p]['role']='core_default_lib'
                elif change=='env': c['final']['environment_overlay']={'DYLD_LIBRARY_PATH':'/wrong'}
                elif change=='selection': c['profiles']['features']['eko']={'default_features':True,'all_features':False,'explicit':[]}
                elif change=='ambiguous': c['descriptor']['final_steps'].append(copy.deepcopy(c['final']))
                elif change=='missing-provenance': c['final'].pop('role_provenance')
                else:
                    path=Path(c['artifacts']['path']);v=json.loads(path.read_bytes());v['roles'][0]['actual_profile_u64_decimal']='7169924071199656000';c['final']['role_provenance'][p]['artifacts']=write(path,v)
                with self.assertRaises((ValueError,REPORT.InvalidEvidence)):qualify(f,c)

    def test_profile_u64_is_exact_and_rejects_float_bool_out_of_range(self):
        for value in (7169924071199656256, '7169924071199656256', '10034064700113017540'):
            self.assertEqual(REPORT.profile_decimal(value),str(value))
        for value in (float(7169924071199656256),True,'18446744073709551616','07169924071199656256'):
            with self.assertRaises(REPORT.InvalidEvidence):REPORT.profile_decimal(value)

    def test_independent_directory_anchor_rejects_preconstruction_mode_inode_drift_and_missing_authority(self):
        bridge=source_bridge(self.f,self)
        for key in ('directory_anchor','directory_postscan'):
            missing=copy.deepcopy(bridge);missing.pop(key)
            with self.assertRaisesRegex(ValueError,'anchor missing'):RUNNER.scan_same_source_bridge(missing)
        wrong=copy.deepcopy(bridge);wrong['directory_anchor']['sha256']='0'*64
        with self.assertRaisesRegex(ValueError,'authority'):RUNNER.scan_same_source_bridge(wrong)
        nested=self.f['formal']['framework']/'nested';nested.chmod(0o700)
        # 在prepare的fresh scan之前改变，不能以scan生成新frozen_state蒙混。
        with self.assertRaisesRegex(ValueError,'directory anchor physical/fullmode drift'):RUNNER.scan_same_source_bridge(bridge)
        nested.chmod(0o755)
        source=self.f['sources']['framework'];directories={'.','nested'}
        with self.assertRaisesRegex(ValueError,'directory anchor physical/fullmode drift'):
            RUNNER.verify_directory_anchor(bridge,source,directories,self.f['formal']['framework'],self.f['roots']['framework'])

    def test_independent_directory_anchor_rejects_fresh_root_inode_replacement(self):
        # setUp为本case建立独立新fixture，不承接mode负控的chmod/ctime。
        bridge=source_bridge(self.f,self)
        source=self.f['sources']['framework'];directories={'.','nested'}
        formal=self.f['formal']['framework'];target=self.f['roots']['framework']
        RUNNER.verify_directory_anchor(bridge,source,directories,formal,target)
        original_inode=formal.stat().st_ino
        retired=formal.with_name(formal.name+'-original-inode')
        formal.rename(retired)
        shutil.copytree(retired,formal)
        self.assertNotEqual(original_inode,formal.stat().st_ino)
        # 直接调用现有公开目录接缝，避免文件inode层先拒绝而掩盖目录身份。
        with self.assertRaisesRegex(ValueError,r'directory anchor physical/fullmode drift: formal/\.'):
            RUNNER.verify_directory_anchor(bridge,source,directories,formal,target)

    def test_observed_cfg_and_unit_identity_cannot_borrow_another_role(self):
        cases=[('core_testutils_lib','drop-cfg'),('core_default_lib','add-cfg'),('gui_lib','wrong-cfg'),('core_testutils_lib','extra'),('core_default_lib','out-dir'),('gui_lib','unit')]
        for role,change in cases:
            with self.subTest(role=role,change=change),tempfile.TemporaryDirectory() as directory:
                f=current_fixture(Path(directory));c=compile_fixture(f,role);record=c['role'];argv=shlex.split(record['actual_final_rustc'][0]['args'])
                if change in ('drop-cfg','wrong-cfg'):
                    while '--cfg' in argv:
                        position=argv.index('--cfg');del argv[position:position+2]
                    if change=='wrong-cfg':argv+=['--cfg','feature="test-utils"']
                elif change=='add-cfg':argv+=['--cfg','feature="test-utils"']
                elif change=='extra':argv[argv.index(next(x for x in argv if x.startswith('extra-filename='))) ]='extra-filename=-badbadbadbadbad0'
                elif change=='out-dir':argv[argv.index('--out-dir')+1]=str(f['root']/'other-output')
                else:argv[argv.index('src/lib.rs')]='source.rs'
                record['actual_final_rustc'][0]['args']=shlex.join(argv)
                updated=write(Path(c['artifacts']['path']),{'NoRun0_W0_postsourceguard0':True,'actual_NoRun_caller':c['final']['receipt'],'roles':[record],'target':c['final']['environment_overlay']['CARGO_TARGET_DIR']})
                c['final']['role_provenance'][str(c['binary'])]['artifacts']=updated
                with self.assertRaises((ValueError,REPORT.InvalidEvidence)):qualify(f,c)

    def test_Git_read_failure_does_not_try_another_source_route(self):
        with mock.patch.object(RUNNER.subprocess,'run',return_value=subprocess.CompletedProcess(['git'],1,b'',b'controlled Git failure')) as called:
            with self.assertRaisesRegex(ValueError,'Git object read failed'):
                RUNNER.source_identity(self.f['roots']['eko'],self.f['supplied']['eko'])
            self.assertEqual(called.call_count,1)


if __name__ == '__main__':
    unittest.main()

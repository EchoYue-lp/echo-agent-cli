"""正式纯native合同的合成小文件控制；无Root归档、程序或App执行。"""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from test_performance_baseline_provenance import REPORT, ref, write

class NativeCaptureContractControls(unittest.TestCase):
    def test_public_bijection_validator_accepts_valid_and_rejects_both_duplicate_counterexamples(self):
        native = REPORT.native_contract();api = REPORT._native_api()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory);cases=[];markers=[]
            for name in ('normal-unknown-1','normal-unknown-2','guard-unknown-1','guard-unknown-2','tool-progress-signed'):
                case={'case_id':name};folder=root/name;folder.mkdir()
                for number in range(8):
                    path=folder/(str(number)+'.json');value=write(path,{'synthetic_reader_control_only':True,'case':name,'member':number})
                    case['member'+str(number)]=value;markers.append(value)
                cases.append(case)
            index_path=root/'capture-index.json';index={'cases':cases};markers.append(write(index_path,index))
            native.validate_capture_members(api,index_path,index,markers)
            for cross_case in (True,False):
                bad=copy.deepcopy(index)
                if cross_case:
                    for number in range(8):bad['cases'][1]['member'+str(number)]=copy.deepcopy(index['cases'][0]['member'+str(number)])
                else:bad['cases'][0]['member1']=copy.deepcopy(index['cases'][0]['member0'])
                with self.subTest(cross_case=cross_case),self.assertRaises(REPORT.InvalidEvidence):
                    native.validate_capture_members(api,index_path,bad,markers)


    def test_outer_success_does_not_hide_failed_or_rebound_child(self):
        native=REPORT.native_contract();api=REPORT._native_api()
        with tempfile.TemporaryDirectory() as directory:
            binary=Path(directory)/'synthetic-program-not-executed';binary.write_text('control only')
            retained={str(binary):ref(binary)['sha256']}
            args=[str(binary),'tauri::commands::conversations::native_history_tests::native_timeline_actual_backend_capture::native_unknown_actual_backend_replay_capture','--exact','--ignored','--nocapture','--test-threads=1']
            start={'argv':args,'binary':{'path':str(binary),'sha256':retained[str(binary)]}}
            process={'args':args,'exit':0};native.validate_original707_child(api,{'exitCode':0},process,start,retained)
            for change in ('exit','args','binary'):
                bad=copy.deepcopy(process)
                if change=='exit':bad['exit']=1
                elif change=='args':bad['args'][1]='wrong-selector'
                else:bad['args'][0]=str(binary)+'.wrong'
                with self.subTest(change=change),self.assertRaises(REPORT.InvalidEvidence):native.validate_original707_child(api,{'exitCode':0},bad,start,retained)

    def test_unknown_capture_wrapper_does_not_select_a_fallback(self):
        with self.assertRaises(REPORT.InvalidEvidence):
            REPORT.native_contract().qualify_original707_capture(REPORT._native_api(),Path(__file__),{'execution_kind':'any-python-wrapper'},{},Path('/not-used'),{},set())

if __name__=='__main__':unittest.main()

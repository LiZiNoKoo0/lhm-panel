import os, subprocess, sys
env = dict(os.environ); env["PYTHONPATH"] = r"H:\newwork"
out = open(r"H:\newwork\firmware\_child.txt", "wb")
subprocess.run([sys.executable, "-c",
  "import sys,os;print('exe',sys.executable);print('cwd',os.getcwd());print('PP',os.environ.get('PYTHONPATH'));print('path',sys.path[:4]);import pcmon;print('pcmon ok',pcmon.__file__)"],
  cwd=r"H:\newwork", env=env, stdout=out, stderr=subprocess.STDOUT)
out.close()
print(open(r"H:\newwork\firmware\_child.txt").read())

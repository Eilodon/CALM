import handlers


def dispatch(method_name):
    fn = getattr(handlers, method_name)
    return fn()

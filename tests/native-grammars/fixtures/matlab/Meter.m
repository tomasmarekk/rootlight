% Native syntax fixture: class, property validation, methods and accessor names.
classdef Meter < handle
    properties
        Value (1,1) double = 0
    end
    methods
        function obj = Meter(value)
            obj.Value = value;
        end
        function result = read(obj)
            result = obj.Value;
        end
        function result = get.Value(obj)
            result = obj.Value;
        end
    end
end
